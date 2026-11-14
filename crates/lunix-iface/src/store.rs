//! Durable object storage on the local file system: segments, map packs,
//! commits, the checkpoint, pins, GC state and the volume directory (M7).
//!
//! Every method here **blocks** (file I/O) and is called from workers, the
//! coordinator or startup code, never from a queue thread, except
//! [`SegmentStore::locate`], which only resolves a hint so the engine can
//! submit the read on its own ring.
//!
//! # Durable order
//!
//! A write that creates an object (`seal`, `put`, `store`, `put_evidence`) is
//! complete when it returns: data `fdatasync`ed, renamed into place, directory
//! `fsync`ed. A delete is `unlink` then directory `fsync`. A crash leaves the
//! old state or the new one, never a mix. Create-only objects are refused with
//! `AlreadyExists` rather than replaced.
//!
//! # Extents
//!
//! The store keeps extents as opaque, already-sealed byte strings; it parses
//! only the fixed prefix it needs to index them. **Verification is the caller's
//! single step**: `read_extent` returns the stored bytes after bounds checks
//! and the caller passes them to [`ExtentCodec::open`](crate::crypto::ExtentCodec::open),
//! which checks the 16-byte tag (D11). Segment and pack *footers* are sealed
//! through the `MetaCodec` the store was built with.

use std::collections::HashSet;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;

use lunix_format::gcfile::Condemned;
use lunix_format::local::{CheckpointSlot, DeviceRecord, LocalPins, RollbackFloor, VolumeJson};
use lunix_format::node::NodeLoc;
use lunix_format::segment::FooterBody;
use lunix_format::{ChunkId, CommitSeq, Hint, ObjId, PackId, SegId, TimeMs, VolumeUuid};

use crate::error::Result;
use crate::journal::WriteSeqRange;

/// Allocates object ids (segments, map packs, index runs share one counter,
/// epoch 0 in phase 1). Ids are never reused, even after a crash (D17).
pub trait IdAllocator: Send + Sync {
    /// The next id, strictly above everything issued or observed.
    ///
    /// # Errors
    /// `Refused` when the counter is exhausted.
    fn allocate(&self) -> Result<ObjId>;

    /// The raw value of the highest id issued or observed (0 if none); this is
    /// what the checkpoint records as `id_high`.
    fn high_water(&self) -> u64;

    /// Raises the floor to at least `raw` (a raw `ObjId` value): startup feeds
    /// it the checkpoint `id_high`, the highest id the chain names and the
    /// highest id on disk before the orphan sweep. Never lowers it.
    fn observe(&self, raw: u64);
}

/// A sealed segment on disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SegmentInfo {
    /// The segment.
    pub seg: SegId,
    /// File size in bytes (what the ledger charges).
    pub size_bytes: u64,
}

/// A sealed map pack on disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PackInfo {
    /// The pack.
    pub pack: PackId,
    /// File size in bytes.
    pub size_bytes: u64,
}

/// A commit object on disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CommitInfo {
    /// Its `commit_seq`.
    pub seq: CommitSeq,
    /// File size in bytes.
    pub size_bytes: u64,
}

/// Where an extent lives, for a read the engine submits on its own io_uring.
#[derive(Clone, Debug)]
pub struct ExtentLocation {
    /// The open segment file; the `Arc` keeps it valid across a delete.
    pub fd: Arc<OwnedFd>,
    /// Byte offset of the extent in the file.
    pub offset: u64,
    /// Whole extent length (`OVH + clen`).
    pub len: u32,
}

/// The result of sealing a segment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SealedSegment {
    /// The segment id.
    pub seg: SegId,
    /// Final file size.
    pub size_bytes: u64,
    /// Extents in it.
    pub extents: u32,
}

/// Appends extents to one open segment (`segments/<seg>.open`). `Send`, used
/// by one thread at a time.
pub trait SegmentWriter: Send {
    /// The id this segment will have.
    fn id(&self) -> SegId;

    /// Appends one complete sealed extent for `chunk_id` and returns its hint
    /// (`off` is the extent's offset, `clen` its payload length). `dict_id` is
    /// the zstd dictionary id of a codec-3 extent (for the footer's `dict_ids`),
    /// `None` otherwise (always in phase 1).
    ///
    /// # Errors
    /// `NoSpace`, `Io`; `Invalid` for bytes that are not an extent.
    fn append_extent(
        &mut self,
        chunk_id: &ChunkId,
        dict_id: Option<u32>,
        extent: &[u8],
    ) -> Result<Hint>;

    /// Bytes appended so far.
    fn len(&self) -> u64;

    /// True when nothing has been appended.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True once the target size (16 MiB) is reached; the caller seals and
    /// opens the next segment.
    fn is_full(&self) -> bool;

    /// Writes the footer and trailer, makes the file durable and renames it to
    /// its sealed name. `writes` is the `write_seq` range of the data in it
    /// (`None` for compaction output). Dropping the writer without sealing
    /// abandons the segment; the file is removed now or by the open-time sweep.
    ///
    /// # Errors
    /// `NoSpace`, `Io`.
    fn seal(self: Box<Self>, writes: Option<WriteSeqRange>) -> Result<SealedSegment>;
}

/// Sealed segments. `Send + Sync`.
pub trait SegmentStore: Send + Sync {
    /// Starts a segment with a fresh id.
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>>;

    /// Appends the stored bytes of the extent at `hint` (`OVH + clen` of them)
    /// to `out`. Only sealed segments are readable; `*.open` files never are.
    ///
    /// # Errors
    /// `NotFound` for a missing segment, `Corrupt` for a hint outside the file.
    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()>;

    /// Resolves a hint to a file and range (non-blocking apart from a possible
    /// first open of the file).
    fn locate(&self, hint: &Hint) -> Result<ExtentLocation>;

    /// The decoded, authenticated footer of a sealed segment.
    ///
    /// # Errors
    /// `NotFound`, `Corrupt`, `Auth`.
    fn footer(&self, seg: SegId) -> Result<FooterBody>;

    /// All sealed segments with sizes, ascending by id.
    fn list(&self) -> Result<Vec<SegmentInfo>>;

    /// Deletes a sealed segment (unlink, directory fsync) and returns the bytes
    /// freed; a missing segment frees 0 (deletes are idempotent). Callers: GC,
    /// the orphan sweep at open, and destage, which deletes the segments it
    /// sealed for a cut that failed before any durable object named them.
    /// Never delete a segment that a durable commit, the index or a pending
    /// re-hint names.
    fn delete(&self, seg: SegId) -> Result<u64>;
}

/// Footers by segment, as the index needs them for `catch_up` and `rebuild`.
/// Implemented for every [`SegmentStore`].
pub trait SegmentFooters {
    /// The footer of `seg`.
    ///
    /// # Errors
    /// `NotFound` when the segment does not exist.
    fn segment_footer(&self, seg: SegId) -> Result<FooterBody>;
}

impl<T: SegmentStore + ?Sized> SegmentFooters for T {
    fn segment_footer(&self, seg: SegId) -> Result<FooterBody> {
        self.footer(seg)
    }
}

/// The result of sealing a map pack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SealedPack {
    /// The pack id.
    pub pack: PackId,
    /// Final file size.
    pub size_bytes: u64,
    /// Nodes in it.
    pub nodes: u32,
}

/// Appends map nodes to one open pack (`map/<pack>.open`).
pub trait PackWrite: Send {
    /// The id this pack will have.
    fn id(&self) -> PackId;

    /// Appends one sealed node extent whose node hash is `hash` and returns its
    /// location (`len` is the whole extent length).
    ///
    /// # Errors
    /// `NoSpace`, `Io`; `Invalid` for bytes that are not a node extent.
    fn append_node(&mut self, hash: &[u8; 32], extent: &[u8]) -> Result<NodeLoc>;

    /// Bytes appended so far.
    fn len(&self) -> u64;

    /// True when nothing has been appended.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Footer, trailer, durability and rename, as for a segment.
    ///
    /// # Errors
    /// `NoSpace`, `Io`.
    fn seal(self: Box<Self>) -> Result<SealedPack>;
}

/// Reading map packs. `Send + Sync`.
pub trait PackRead: Send + Sync {
    /// Appends the stored bytes of the node extent at `loc` to `out`.
    ///
    /// # Errors
    /// `NotFound` (pack missing), `Corrupt` (range outside the pack).
    fn read_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()>;

    /// The footer of a sealed pack.
    fn pack_footer(&self, pack: PackId) -> Result<FooterBody>;

    /// All sealed packs with sizes, ascending.
    fn list_packs(&self) -> Result<Vec<PackInfo>>;
}

/// Map packs, readable and writable.
pub trait PackStore: PackRead {
    /// Starts a pack with a fresh id.
    fn open_pack(&self) -> Result<Box<dyn PackWrite>>;

    /// Deletes a whole pack (map packs are never rewritten in phase 1) and
    /// returns the bytes freed; missing frees 0.
    fn delete_pack(&self, pack: PackId) -> Result<u64>;
}

/// Commit objects (`state/commits/<commit_seq:016x>`), stored as the sealed or
/// MAC'd bytes the `CommitCodec` produced. The store never interprets them.
pub trait CommitStore: Send + Sync {
    /// Creates commit `seq` durably. Create-only.
    ///
    /// # Errors
    /// `AlreadyExists`, `NoSpace`, `Io`.
    fn put(&self, seq: CommitSeq, object: &[u8]) -> Result<()>;

    /// The stored bytes of commit `seq`.
    ///
    /// # Errors
    /// `NotFound`.
    fn get(&self, seq: CommitSeq) -> Result<Vec<u8>>;

    /// All commit objects, ascending by `seq`.
    fn list(&self) -> Result<Vec<CommitInfo>>;

    /// The highest `seq` present, if any.
    fn tip(&self) -> Result<Option<CommitSeq>>;

    /// Deletes commit `seq`; returns the bytes freed (missing frees 0).
    /// `chain_floor` is the `chain_floor` of the **authenticated tip**, which
    /// the store cannot read itself (commits may be sealed). Refused
    /// (`Refused`, nothing changes) when `seq >= chain_floor` or `seq` is the
    /// current tip.
    fn delete(&self, seq: CommitSeq, chain_floor: CommitSeq) -> Result<u64>;

    /// `fsck --repair --truncate-tip`: renames the current tip aside as
    /// `<seq>.damaged` (durable, never deleted) so that the previous commit
    /// becomes the tip.
    ///
    /// # Errors
    /// `Refused` when `seq` is not the tip; `NotFound`.
    fn set_aside(&self, seq: CommitSeq) -> Result<()>;
}

/// The checkpoint hint (`state/checkpoint`): two checksummed slots.
pub trait CheckpointStore: Send + Sync {
    /// The newest valid slot. A missing, torn or stale file is `Ok(None)` (with
    /// a warning from the implementation): the checkpoint is only a hint.
    fn load(&self) -> Result<Option<CheckpointSlot>>;

    /// Writes the older slot durably. The store assigns `slot_seq` = newest + 1
    /// and ignores the value in `slot`.
    fn store(&self, slot: &CheckpointSlot) -> Result<()>;

    /// Zeroes both slots (`--truncate-tip`, D17).
    fn zero(&self) -> Result<()>;
}

/// What kind of pin a root comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PinKind {
    /// `snapshots/<name>`.
    Snapshot,
    /// `readers/<host>-<id>`.
    Reader,
    /// `holds/<child-uuid>`.
    Hold,
}

/// A commit that a pin keeps alive.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PinRoot {
    /// The kind of pin.
    pub kind: PinKind,
    /// The pin's object name (without the directory).
    pub name: String,
    /// The `commit_seq` it pins.
    pub commit_seq: CommitSeq,
    /// When a reader pin lapses; `None` for the others.
    pub deadline: Option<TimeMs>,
}

/// The local `snapshots/`, `readers/` and `holds/` directories. All empty in
/// phase 1, but GC reads them so no version can delete a future pin's base.
pub trait PinSource: Send + Sync {
    /// Every pin, authenticated. An undecodable pin is an error, not a skip:
    /// GC must not run past a pin it cannot read.
    fn roots(&self) -> Result<Vec<PinRoot>>;
}

/// Local GC state (`state/gc/`, `gc/`).
pub trait GcStateStore: Send + Sync {
    /// The condemned list; `Ok(None)` when absent or undecodable (it is a hint:
    /// the caller re-marks).
    fn load_condemned(&self) -> Result<Option<Condemned>>;

    /// Replaces the condemned list atomically (tmp, fsync, rename, dir fsync).
    fn store_condemned(&self, list: &Condemned) -> Result<()>;

    /// Writes `gc/<pass:016x>`: the pass's JSON `lines` wrapped as the frozen
    /// `{ver: 1, bytes}` body and sealed through the `MetaCodec`. Create-only
    /// and atomic.
    ///
    /// # Errors
    /// `AlreadyExists` for a second call with the same `pass`.
    fn put_evidence(&self, pass: u64, lines: &[u8]) -> Result<()>;

    /// The pass numbers that have an evidence object, ascending (the next pass
    /// is one above the last).
    fn evidence_passes(&self) -> Result<Vec<u64>>;
}

/// What the open-time orphan sweep (N2) keeps.
#[derive(Clone, Copy, Debug)]
pub struct SweepRequest<'a> {
    /// Raw value of the highest id any retained commit names.
    pub high_water_mark: u64,
    /// Segments some retained commit names.
    pub named_segments: &'a HashSet<SegId>,
    /// Map packs some retained commit names.
    pub named_packs: &'a HashSet<PackId>,
}

/// What a sweep did.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SweepReport {
    /// Raw value of the highest id present on disk **before** anything was
    /// deleted; feed it to [`IdAllocator::observe`] (D17).
    pub highest_id_before: u64,
    /// Sealed segments above the mark that nothing names, deleted.
    pub deleted_segments: Vec<SegId>,
    /// Sealed packs above the mark that nothing names, deleted.
    pub deleted_packs: Vec<PackId>,
    /// `*.tmp` and `*.open` files deleted.
    pub deleted_temp: u32,
    /// Unreferenced sealed files at or below the mark; left for GC.
    pub kept_unreferenced: Vec<ObjId>,
}

/// A temporary or unsealed file found in the volume directory (`*.tmp`,
/// `*.open`), which no reader ever uses.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Leftover {
    /// Path relative to the volume directory.
    pub name: String,
    /// Size in bytes.
    pub size_bytes: u64,
}

/// Why the rollback floor is reset (see [`VolumeDir::reset_rollback_floor`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum FloorReset {
    /// `fsck --repair --truncate-tip`: the new tip is the previous commit.
    TruncateTip,
    /// `fsck --repair --reset-floor`: the operator's explicit reset (for
    /// example a DR steal from a lagging mirror, phase 5).
    OperatorReset,
    /// `fsck --repair --repair-pins`: replaces a rejected `state/pins`.
    RepairPins,
}

impl FloorReset {
    /// The word written to the audit record.
    pub fn word(self) -> &'static str {
        match self {
            Self::TruncateTip => "truncate_tip",
            Self::OperatorReset => "operator_reset",
            Self::RepairPins => "repair_pins",
        }
    }
}

/// The volume directory. Opening it takes the exclusive `LOCK` (with stale-lock
/// takeover); the lock is held until the value is dropped.
pub trait VolumeDir: Send + Sync {
    /// The volume's uuid.
    fn uuid(&self) -> VolumeUuid;

    /// The volume directory (keyring and `integrity.key` paths, fsck output).
    fn dir(&self) -> &Path;

    /// The exact bytes of the `volume` descriptor.
    fn volume_bytes(&self) -> Result<Vec<u8>>;

    /// `volume.json`.
    fn volume_json(&self) -> Result<VolumeJson>;

    /// Replaces `volume.json` atomically.
    fn store_volume_json(&self, json: &VolumeJson) -> Result<()>;

    /// `state/device`; `None` when absent.
    fn device_record(&self) -> Result<Option<DeviceRecord>>;

    /// Writes `state/device` atomically.
    fn store_device_record(&self, rec: &DeviceRecord) -> Result<()>;

    /// Removes `state/device`.
    fn clear_device_record(&self) -> Result<()>;

    /// `state/pins`; `None` when absent.
    fn local_pins(&self) -> Result<Option<LocalPins>>;

    /// Writes `state/pins` atomically. The rollback floor already recorded
    /// (see [`rollback_floor`](Self::rollback_floor)) is kept: `LocalPins`
    /// does not carry it, and replacing the pins never lowers it.
    fn store_local_pins(&self, pins: &LocalPins) -> Result<()>;

    /// The rollback floor `{max_commit_seq, tip_hash}` of `state/pins`
    /// version 2 (ADR-31, rule R4); `None` when no floor was ever recorded,
    /// or when `state/pins` is absent. Required (no
    /// default): a silent `None` would make every rollback pass R4.
    ///
    /// # Errors
    /// `Io`, `Corrupt` for an unreadable `state/pins`.
    fn rollback_floor(&self) -> Result<Option<RollbackFloor>>;

    /// Raises the rollback floor, keeping the rest of `state/pins`. The new
    /// floor is durable (temporary file, `fdatasync`, rename, directory
    /// `fsync`) when this returns `Ok`. When `state/pins` is absent it is
    /// created with zero monotonic floors and no pin.
    ///
    /// The floor is raise-only. A `floor` equal to the recorded one is a
    /// no-op. A lower `max_commit_seq` is refused, and so is an **equal**
    /// `max_commit_seq` with a different `tip_hash`, so that a buggy caller
    /// cannot turn a floor into a fork. Only offline
    /// `fsck --repair --truncate-tip` and an explicit operator reset lower
    /// it, and they do not use this method.
    ///
    /// # Errors
    /// `Refused` for a lower `max_commit_seq` or an equal one with another
    /// hash (nothing changes); `Io`.
    fn raise_rollback_floor(&self, floor: &RollbackFloor) -> Result<()>;

    /// Sets the rollback floor to `floor`, **lowering** it when the recorded
    /// one is higher (or has another hash at that height), and returns the
    /// floor that was recorded before (`None`: none, or a rejected file for
    /// [`FloorReset::RepairPins`]). ADR-31 names exactly the offline callers:
    /// `fsck --repair --truncate-tip`, the operator's explicit reset and the
    /// repair of a rejected `state/pins`, all in `lunix-fsck`. The engine and the
    /// daemon never call it.
    ///
    /// The store checks what it can before it changes anything: the commit at
    /// `floor.max_commit_seq` must exist and its stored bytes must hash
    /// (BLAKE3-256) to `floor.tip_hash`, so a caller with stale facts cannot
    /// record a floor that makes the volume unmountable. For
    /// [`FloorReset::TruncateTip`] the new height must also be **below** the
    /// recorded one. For [`FloorReset::RepairPins`] `state/pins` must really be
    /// rejected; it is then replaced by a fresh file (zero `max_keyring_seq`
    /// and `max_epoch`, no pin, the new floor), and those three values are
    /// given up.
    ///
    /// Every reset appends one line (time, reason, old and new floor) to
    /// `state/floor-resets.log` and makes it durable **before** `state/pins`
    /// changes. The write of `state/pins` is durable as for
    /// [`raise_rollback_floor`](Self::raise_rollback_floor), and the rest of
    /// the file is kept.
    ///
    /// # Errors
    /// `Refused` unless the store was opened with offline repair enabled
    /// (`StoreOptions::allow_floor_reset` in `lunix-store`; only `lunix-fsck`
    /// sets it), when the commit at the new height is missing or hashes to
    /// something else, for the height rule above, or when `RepairPins` finds
    /// a readable `state/pins`; `Corrupt` for a rejected `state/pins` (not
    /// `RepairPins`); `Io`.
    fn reset_rollback_floor(
        &self,
        floor: &RollbackFloor,
        why: FloorReset,
    ) -> Result<Option<RollbackFloor>>;

    /// The shared object-id allocator.
    fn ids(&self) -> Arc<dyn IdAllocator>;

    /// Lists `*.tmp` and `*.open` files (fsck reports them as crash leftovers).
    fn leftovers(&self) -> Result<Vec<Leftover>>;

    /// Deletes every leftover (`fsck --repair`); returns how many.
    fn remove_leftovers(&self) -> Result<u32>;

    /// The open-time sweep: deletes `*.tmp`, `*.open` and every sealed segment
    /// and pack **above** `high_water_mark` that no retained commit names.
    /// Runs before `ChunkIndex::catch_up`. Idempotent.
    fn sweep_orphans(&self, req: &SweepRequest<'_>) -> Result<SweepReport>;
}
