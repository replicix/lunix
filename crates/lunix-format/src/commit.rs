//! Commits (freeze item 6, on-disk-formats.md section 10).
//!
//! A commit is a metadata envelope whose body is one of five kinds. This
//! module frames and parses the bodies; MACs and sealing belong to
//! `lunix-crypto`, so the object helpers take the 32-byte `mac` (or the sealed
//! parts) from the caller.

use crate::bytes::{Put, Reader};
use crate::envelope::{self, Envelope, EnvelopeBody};
use crate::error::{FormatError, Result};
use crate::ids::{CommitSeq, Epoch, ObjId, PackId, RunId, SegId, TimeMs, WriteSeq};
use crate::mode::{Form, ObjectKind};
use crate::paths::valid_backend_id;

const WHAT: &str = "commit";

/// Commit body version.
pub const BODY_VER: u8 = 1;
/// Length of the common header.
pub const COMMON_LEN: usize = 72;
/// Maximum map packs a FULL names.
pub const MAX_MAP_PACKS: u32 = 1024;
/// Maximum new segments a FULL names.
pub const MAX_NEW_SEGMENTS: u32 = 262_144;
/// Maximum index runs a FULL names.
pub const MAX_INDEX_PACKS: u32 = 4096;
/// Maximum refs of a DELTA.
pub const MAX_REFS: u32 = 4096;
/// Maximum stored or decoded DELTA records region.
pub const MAX_RECORDS: u32 = 64 << 20;
/// Maximum backends of a CONFIG.
pub const MAX_BACKENDS: u32 = 64;
/// Maximum CONFIG policy length.
pub const MAX_POLICY: u32 = 65_536;
/// Maximum backend id length.
pub const MAX_ID_LEN: usize = 64;
/// Maximum backend url length.
pub const MAX_URL_LEN: usize = 2048;
/// Largest whole extent of a map node (`OVH` 108 + 65 536).
pub const MAX_NODE_EXTENT: u32 = 65_644;
/// Largest device size.
pub const MAX_SIZE: u64 = 1 << 56;

/// The five commit kinds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CommitKind {
    /// A full commit with a map root.
    Full = 1,
    /// Inline records between FULLs.
    Delta = 2,
    /// An epoch change.
    Epoch = 3,
    /// A backend configuration.
    Config = 4,
    /// An authority transfer.
    Transfer = 5,
}

impl CommitKind {
    /// Parses the `commit_kind` byte.
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(Self::Full),
            2 => Ok(Self::Delta),
            3 => Ok(Self::Epoch),
            4 => Ok(Self::Config),
            5 => Ok(Self::Transfer),
            _ => Err(FormatError::range(WHAT, "commit_kind", u64::from(v))),
        }
    }

    /// The envelope kind: DELTA is kind 3 (`delta`), the rest kind 1.
    pub fn object_kind(self) -> ObjectKind {
        if self == Self::Delta {
            ObjectKind::Delta
        } else {
            ObjectKind::Commit
        }
    }
}

/// The 72-byte header every commit body starts with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CommonHeader {
    /// Which body follows.
    pub kind: CommitKind,
    /// Position in the chain; 0 is genesis.
    pub commit_seq: CommitSeq,
    /// Owner epoch; 0 in phase 1.
    pub epoch: Epoch,
    /// BLAKE3-256 of the parent object as stored (genesis: of `volume`).
    pub parent_hash: [u8; 32],
    /// Writer clamp `max(now, parent.created_at)`; readers never reject on it.
    pub created_at: TimeMs,
}

impl CommonHeader {
    /// `parent_seq` as stored: `commit_seq - 1`, or `None` for genesis.
    pub fn parent_seq(&self) -> Option<CommitSeq> {
        self.commit_seq.0.checked_sub(1).map(CommitSeq)
    }

    fn encode(&self, out: &mut Vec<u8>, has_heat: bool) {
        let parent = self.parent_seq();
        let flags = u16::from(parent.is_some()) | u16::from(has_heat) << 1;
        out.put_u8(BODY_VER);
        out.put_u8(self.kind as u8);
        out.put_u16(flags);
        out.put_zeros(4);
        out.put_u64(self.commit_seq.0);
        out.put_u64(self.epoch.0);
        out.put_u64(parent.map_or(0, |p| p.0));
        out.extend_from_slice(&self.parent_hash);
        out.put_u64(self.created_at.0);
    }

    fn decode(r: &mut Reader<'_>) -> Result<(Self, bool)> {
        r.version_u8(BODY_VER)?;
        let kind = CommitKind::from_u8(r.u8()?)?;
        let flags = r.u16()?;
        if flags & !0b11 != 0 {
            return Err(FormatError::Reserved { what: WHAT });
        }
        let has_parent = flags & 1 != 0;
        let has_heat = flags & 2 != 0;
        r.reserved(4)?;
        let commit_seq = r.u64()?;
        let epoch = r.u64()?;
        let parent_seq = r.u64()?;
        let parent_hash = r.array::<32>()?;
        let created_at = r.u64()?;
        if has_parent != (commit_seq != 0) {
            return Err(FormatError::invalid(
                WHAT,
                "has_parent must be clear exactly at genesis",
            ));
        }
        if parent_seq != commit_seq.saturating_sub(1) {
            return Err(FormatError::invalid(
                WHAT,
                "parent_seq is not commit_seq - 1",
            ));
        }
        if has_heat && kind != CommitKind::Full {
            return Err(FormatError::Reserved { what: WHAT });
        }
        Ok((
            Self {
                kind,
                commit_seq: CommitSeq(commit_seq),
                epoch: Epoch(epoch),
                parent_hash,
                created_at: TimeMs(created_at),
            },
            has_heat,
        ))
    }

    fn expect(&self, kind: CommitKind) -> Result<()> {
        if self.kind == kind {
            Ok(())
        } else {
            Err(FormatError::invalid(
                WHAT,
                "commit_kind does not match the body type",
            ))
        }
    }
}

/// Location of the map root's extent, carried by the commit because the root
/// has no parent entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MapRoot {
    /// Root node hash (`E_{levels-1}` for an empty map).
    pub hash: [u8; 32],
    /// Pack holding the root; `None` iff the map is empty.
    pub pack: Option<PackId>,
    /// Offset of the root's extent in the pack; 0 if empty.
    pub off: u32,
    /// Whole extent length (`OVH + clen`); 0 if empty.
    pub len: u32,
}

impl MapRoot {
    /// The root of an empty map.
    pub fn empty(hash: [u8; 32]) -> Self {
        Self {
            hash,
            pack: None,
            off: 0,
            len: 0,
        }
    }

    fn validate(&self) -> Result<()> {
        match self.pack {
            None if self.off != 0 || self.len != 0 => {
                Err(FormatError::invalid(WHAT, "empty map root has a location"))
            }
            Some(_) if self.len == 0 || self.len > MAX_NODE_EXTENT => Err(FormatError::range(
                WHAT,
                "map_root.len",
                u64::from(self.len),
            )),
            _ => Ok(()),
        }
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        out.extend_from_slice(&self.hash);
        out.put_u64(self.pack.map_or(0, ObjId::raw));
        out.put_u32(self.off);
        out.put_u24(self.len);
        out.put_u8(0);
        Ok(())
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let hash = r.array::<32>()?;
        let pack = ObjId::from_raw_opt(r.u64()?)?;
        let off = r.u32()?;
        let len = r.u24()?;
        if r.u8()? != 0 {
            return Err(FormatError::Reserved { what: WHAT });
        }
        let m = Self {
            hash,
            pack,
            off,
            len,
        };
        m.validate()?;
        Ok(m)
    }
}

fn check_size(size: u64) -> Result<()> {
    if !(4096..=MAX_SIZE).contains(&size) || !size.is_multiple_of(4096) {
        return Err(FormatError::range(WHAT, "size", size));
    }
    Ok(())
}

fn encode_ids(out: &mut Vec<u8>, field: &'static str, ids: &[ObjId], max: u32) -> Result<()> {
    if ids.len() as u64 > u64::from(max) {
        return Err(FormatError::count(
            WHAT,
            field,
            ids.len() as u64,
            u64::from(max),
        ));
    }
    if ids.windows(2).any(|w| w[0] >= w[1]) {
        return Err(FormatError::not_canonical(
            WHAT,
            "id list is not strictly ascending",
        ));
    }
    for id in ids {
        out.put_u64(id.raw());
    }
    Ok(())
}

fn read_ids(r: &mut Reader<'_>, field: &'static str, n: u32, max: u32) -> Result<Vec<ObjId>> {
    let n = r.check_count(field, u64::from(n), u64::from(max), 8)?;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let id = ObjId::from_raw(r.u64()?)?;
        if v.last().is_some_and(|&p| p >= id) {
            return Err(FormatError::not_canonical(
                WHAT,
                "id list is not strictly ascending",
            ));
        }
        v.push(id);
    }
    Ok(v)
}

/// FULL commit body (kind 1).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Full {
    /// Common header; `kind` must be [`CommitKind::Full`].
    pub common: CommonHeader,
    /// The cut: highest `write_seq` this FULL covers.
    pub cut_seq: WriteSeq,
    /// Device size in bytes.
    pub size: u64,
    /// `commit_seq` of the oldest retained FULL.
    pub chain_floor: CommitSeq,
    /// The map root.
    pub map_root: MapRoot,
    /// `commit_seq` of `heat/<n>`, when the FULL has one.
    pub heat: Option<CommitSeq>,
    /// Map packs this commit introduced.
    pub map_packs: Vec<PackId>,
    /// Segments this commit introduced.
    pub new_segments: Vec<SegId>,
    /// Index runs durable before C6 of this commit (D3).
    pub index_packs: Vec<RunId>,
}

impl Full {
    /// The genesis commit of D5: seq 0, epoch 0, empty map and lists.
    pub fn genesis(
        volume_hash: [u8; 32],
        size: u64,
        empty_root_hash: [u8; 32],
        created_at: TimeMs,
    ) -> Self {
        Self {
            common: CommonHeader {
                kind: CommitKind::Full,
                commit_seq: CommitSeq(0),
                epoch: Epoch(0),
                parent_hash: volume_hash,
                created_at,
            },
            cut_seq: WriteSeq(0),
            size,
            chain_floor: CommitSeq(0),
            map_root: MapRoot::empty(empty_root_hash),
            heat: None,
            map_packs: Vec::new(),
            new_segments: Vec::new(),
            index_packs: Vec::new(),
        }
    }

    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.common.expect(CommitKind::Full)?;
        check_size(self.size)?;
        if self.chain_floor > self.common.commit_seq {
            return Err(FormatError::invalid(WHAT, "chain_floor above commit_seq"));
        }
        let start = out.len();
        let res = self.encode_inner(out);
        if res.is_err() {
            out.truncate(start);
        }
        res
    }

    fn encode_inner(&self, out: &mut Vec<u8>) -> Result<()> {
        self.common.encode(out, self.heat.is_some());
        out.put_u64(self.cut_seq.0);
        out.put_u64(self.size);
        out.put_u64(self.chain_floor.0);
        self.map_root.encode(out)?;
        out.put_u64(self.heat.map_or(0, |h| h.0));
        for (field, n, max) in [
            ("n_map_packs", self.map_packs.len(), MAX_MAP_PACKS),
            ("n_new_segments", self.new_segments.len(), MAX_NEW_SEGMENTS),
            ("n_index_packs", self.index_packs.len(), MAX_INDEX_PACKS),
        ] {
            if n as u64 > u64::from(max) {
                return Err(FormatError::count(WHAT, field, n as u64, u64::from(max)));
            }
            out.put_u32(n as u32);
        }
        out.put_zeros(4);
        encode_ids(out, "map_packs", &self.map_packs, MAX_MAP_PACKS)?;
        encode_ids(out, "new_segments", &self.new_segments, MAX_NEW_SEGMENTS)?;
        encode_ids(out, "index_packs", &self.index_packs, MAX_INDEX_PACKS)
    }

    /// Parses a whole body.
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut r = Reader::new(body, "commit full");
        let (common, has_heat) = CommonHeader::decode(&mut r)?;
        common.expect(CommitKind::Full)?;
        let cut_seq = WriteSeq(r.u64()?);
        let size = r.u64()?;
        check_size(size)?;
        let chain_floor = CommitSeq(r.u64()?);
        if chain_floor > common.commit_seq {
            return Err(FormatError::invalid(WHAT, "chain_floor above commit_seq"));
        }
        let map_root = MapRoot::decode(&mut r)?;
        let heat_seq = r.u64()?;
        if !has_heat && heat_seq != 0 {
            return Err(FormatError::invalid(WHAT, "heat_seq without has_heat"));
        }
        let n_packs = r.u32()?;
        let n_segs = r.u32()?;
        let n_runs = r.u32()?;
        r.reserved(4)?;
        let map_packs = read_ids(&mut r, "n_map_packs", n_packs, MAX_MAP_PACKS)?;
        let new_segments = read_ids(&mut r, "n_new_segments", n_segs, MAX_NEW_SEGMENTS)?;
        let index_packs = read_ids(&mut r, "n_index_packs", n_runs, MAX_INDEX_PACKS)?;
        r.finish()?;
        Ok(Self {
            common,
            cut_seq,
            size,
            chain_floor,
            map_root,
            heat: has_heat.then_some(CommitSeq(heat_seq)),
            map_packs,
            new_segments,
            index_packs,
        })
    }
}

/// Codec of a DELTA's records region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecordsCodec {
    /// Stored as is.
    Raw = 0,
    /// One zstd frame.
    Zstd = 1,
}

/// What a DELTA ref points at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RefKind {
    /// A segment.
    Segment = 1,
    /// A journal object (reserved and unused in v1, ADR-28).
    Journal = 2,
}

/// One entry of a DELTA's `refs[]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeltaRef {
    /// Kind of the referenced object.
    pub kind: RefKind,
    /// Object id (the `ObjId` of a segment).
    pub id: u64,
    /// BLAKE3-256 of the referenced object as stored.
    pub hash: [u8; 32],
}

/// DELTA commit body (kind 2, envelope kind 3).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Delta {
    /// Common header; `kind` must be [`CommitKind::Delta`].
    pub common: CommonHeader,
    /// Cut of the newest FULL or DELTA below this commit.
    pub prev_cut_seq: WriteSeq,
    /// Highest `write_seq` of the flush group; above `prev_cut_seq`.
    pub cut_seq: WriteSeq,
    /// Device size at this cut.
    pub size: u64,
    /// How `records` is stored.
    pub records_codec: RecordsCodec,
    /// Decoded length of the records region.
    pub records_ulen: u32,
    /// The stored records region (journal framing, F1's codec).
    pub records: Vec<u8>,
    /// Referenced objects.
    pub refs: Vec<DeltaRef>,
}

impl Delta {
    fn validate(&self) -> Result<()> {
        self.common.expect(CommitKind::Delta)?;
        if self.cut_seq <= self.prev_cut_seq {
            return Err(FormatError::invalid(WHAT, "cut_seq not above prev_cut_seq"));
        }
        check_size(self.size)?;
        if self.records.len() as u64 > u64::from(MAX_RECORDS) {
            return Err(FormatError::count(
                WHAT,
                "records_len",
                self.records.len() as u64,
                u64::from(MAX_RECORDS),
            ));
        }
        check_records(
            self.records_codec,
            self.records.len() as u32,
            self.records_ulen,
        )?;
        if self.refs.len() as u64 > u64::from(MAX_REFS) {
            return Err(FormatError::count(
                WHAT,
                "n_refs",
                self.refs.len() as u64,
                u64::from(MAX_REFS),
            ));
        }
        Ok(())
    }

    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        self.common.encode(out, false);
        out.put_u64(self.prev_cut_seq.0);
        out.put_u64(self.cut_seq.0);
        out.put_u64(self.size);
        out.put_u32(self.records.len() as u32);
        out.put_u32(self.records_ulen);
        out.put_u16(self.refs.len() as u16);
        out.put_u8(self.records_codec as u8);
        out.put_zeros(5);
        for e in &self.refs {
            out.put_u8(e.kind as u8);
            out.put_zeros(7);
            out.put_u64(e.id);
            out.extend_from_slice(&e.hash);
        }
        out.extend_from_slice(&self.records);
        Ok(())
    }

    /// Parses a whole body. The records region is not decompressed here (the
    /// declared sizes are checked; see [`Delta::records_decoded`]).
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut r = Reader::new(body, "commit delta");
        let (common, _) = CommonHeader::decode(&mut r)?;
        common.expect(CommitKind::Delta)?;
        let prev_cut_seq = WriteSeq(r.u64()?);
        let cut_seq = WriteSeq(r.u64()?);
        if cut_seq <= prev_cut_seq {
            return Err(FormatError::invalid(WHAT, "cut_seq not above prev_cut_seq"));
        }
        let size = r.u64()?;
        check_size(size)?;
        let records_len = r.u32()?;
        let records_ulen = r.u32()?;
        let n_refs = r.u16()?;
        let codec = match r.u8()? {
            0 => RecordsCodec::Raw,
            1 => RecordsCodec::Zstd,
            v => return Err(FormatError::range(WHAT, "records_codec", u64::from(v))),
        };
        r.reserved(5)?;
        check_records(codec, records_len, records_ulen)?;
        let n = r.check_count("n_refs", u64::from(n_refs), u64::from(MAX_REFS), 48)?;
        let mut refs = Vec::with_capacity(n);
        for _ in 0..n {
            let kind = match r.u8()? {
                1 => RefKind::Segment,
                2 => RefKind::Journal,
                v => return Err(FormatError::range(WHAT, "ref kind", u64::from(v))),
            };
            r.reserved(7)?;
            let id = r.u64()?;
            let hash = r.array::<32>()?;
            refs.push(DeltaRef { kind, id, hash });
        }
        r.check_count(
            "records_len",
            u64::from(records_len),
            u64::from(MAX_RECORDS),
            1,
        )?;
        let records = r.take(records_len as usize)?.to_vec();
        r.finish()?;
        Ok(Self {
            common,
            prev_cut_seq,
            cut_seq,
            size,
            records_codec: codec,
            records_ulen,
            records,
            refs,
        })
    }

    /// The decoded records region, bounded by `records_ulen`.
    pub fn records_decoded(&self) -> Result<Vec<u8>> {
        match self.records_codec {
            RecordsCodec::Raw => Ok(self.records.clone()),
            RecordsCodec::Zstd => {
                let out = zstd::bulk::decompress(&self.records, self.records_ulen as usize)
                    .map_err(|_| FormatError::Compression {
                        what: WHAT,
                        why: "records frame",
                    })?;
                if out.len() != self.records_ulen as usize {
                    return Err(FormatError::invalid(
                        WHAT,
                        "records_ulen differs from the frame",
                    ));
                }
                Ok(out)
            }
        }
    }
}

fn check_records(codec: RecordsCodec, len: u32, ulen: u32) -> Result<()> {
    if len > MAX_RECORDS || ulen > MAX_RECORDS {
        return Err(FormatError::range(
            WHAT,
            "records_len",
            u64::from(len.max(ulen)),
        ));
    }
    match codec {
        RecordsCodec::Raw if len != ulen => Err(FormatError::invalid(
            WHAT,
            "raw records: records_ulen != records_len",
        )),
        RecordsCodec::Zstd if len == 0 || ulen == 0 => {
            Err(FormatError::invalid(WHAT, "zstd records region is empty"))
        }
        _ => Ok(()),
    }
}

fn put_id(out: &mut Vec<u8>, field: &'static str, s: &str, min: usize, max: usize) -> Result<()> {
    if s.len() < min || s.len() > max || !valid_backend_id(s) {
        return Err(FormatError::BadString { what: WHAT, field });
    }
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn read_id(r: &mut Reader<'_>, field: &'static str, len: usize) -> Result<String> {
    let s = r.string(field, len)?;
    if !valid_backend_id(s) {
        return Err(FormatError::BadString { what: WHAT, field });
    }
    Ok(s.to_owned())
}

/// EPOCH commit body (kind 3).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EpochCommit {
    /// Common header; `epoch` is the new epoch.
    pub common: CommonHeader,
    /// The backend that holds authority, if named.
    pub authority: Option<String>,
}

impl EpochCommit {
    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.common.expect(CommitKind::Epoch)?;
        let start = out.len();
        self.common.encode(out, false);
        let a = self.authority.as_deref().unwrap_or("");
        let res = (|| {
            out.put_u16(a.len() as u16);
            out.put_zeros(6);
            if self.authority.is_some() {
                put_id(out, "authority", a, 1, MAX_ID_LEN)?;
            }
            Ok(())
        })();
        if res.is_err() {
            out.truncate(start);
        }
        res
    }

    /// Parses a whole body.
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut r = Reader::new(body, "commit epoch");
        let (common, _) = CommonHeader::decode(&mut r)?;
        common.expect(CommitKind::Epoch)?;
        let n = usize::from(r.u16()?);
        if n > MAX_ID_LEN {
            return Err(FormatError::range(WHAT, "authority_len", n as u64));
        }
        r.reserved(6)?;
        let authority = if n == 0 {
            None
        } else {
            Some(read_id(&mut r, "authority", n)?)
        };
        r.finish()?;
        Ok(Self { common, authority })
    }
}

/// Role of a backend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackendRole {
    /// Holds authority.
    Authority = 1,
    /// A mirror.
    Mirror = 2,
}

/// State of a backend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackendState {
    /// Joining.
    Joining = 1,
    /// Catching up.
    CatchingUp = 2,
    /// In sync.
    InSync = 3,
    /// Draining.
    Draining = 4,
    /// Removed.
    Removed = 5,
}

/// One backend of a CONFIG.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Backend {
    /// Backend id, `[a-z0-9._-]`, 1 ..= 64 bytes.
    pub id: String,
    /// Backend url, 1 ..= 2048 bytes of UTF-8.
    pub url: String,
    /// Role.
    pub role: BackendRole,
    /// State.
    pub state: BackendState,
    /// Commit at which the state began.
    pub since_seq: CommitSeq,
}

/// CONFIG commit body (kind 4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConfigCommit {
    /// Common header.
    pub common: CommonHeader,
    /// Rises by 1 per CONFIG or TRANSFER; at least 1.
    pub config_version: u64,
    /// The volume is detached from its backends (ADR-26).
    pub detached: bool,
    /// Authority backend id; empty only when detached with no backends.
    pub authority: String,
    /// Version of the policy body; 0 iff `policy` is empty.
    pub policy_ver: u16,
    /// Backends sorted by id, ids unique.
    pub backends: Vec<Backend>,
    /// Opaque policy body.
    pub policy: Vec<u8>,
}

impl ConfigCommit {
    fn validate(&self) -> Result<()> {
        self.common.expect(CommitKind::Config)?;
        if self.config_version == 0 {
            return Err(FormatError::range(WHAT, "config_version", 0));
        }
        if self.backends.len() as u64 > u64::from(MAX_BACKENDS) {
            return Err(FormatError::count(
                WHAT,
                "n_backends",
                self.backends.len() as u64,
                u64::from(MAX_BACKENDS),
            ));
        }
        if self.policy.len() as u64 > u64::from(MAX_POLICY) {
            return Err(FormatError::count(
                WHAT,
                "policy_len",
                self.policy.len() as u64,
                u64::from(MAX_POLICY),
            ));
        }
        if (self.policy_ver == 0) != self.policy.is_empty() {
            return Err(FormatError::invalid(
                WHAT,
                "policy_ver is 0 exactly when the policy is empty",
            ));
        }
        if self.authority.is_empty() {
            if !(self.detached && self.backends.is_empty()) {
                return Err(FormatError::invalid(
                    WHAT,
                    "empty authority needs a detached volume without backends",
                ));
            }
        } else if !valid_backend_id(&self.authority) {
            return Err(FormatError::BadString {
                what: WHAT,
                field: "authority",
            });
        }
        if self.backends.windows(2).any(|w| w[0].id >= w[1].id) {
            return Err(FormatError::not_canonical(
                WHAT,
                "backends not sorted by unique id",
            ));
        }
        let n_auth = self
            .backends
            .iter()
            .filter(|b| b.role == BackendRole::Authority)
            .count();
        if n_auth > 1 || (n_auth == 0 && !self.detached) {
            return Err(FormatError::invalid(
                WHAT,
                "exactly one authority backend unless detached",
            ));
        }
        if !self.authority.is_empty()
            && !self
                .backends
                .iter()
                .any(|b| b.role == BackendRole::Authority && b.id == self.authority)
        {
            return Err(FormatError::invalid(
                WHAT,
                "authority does not name an authority backend",
            ));
        }
        Ok(())
    }

    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        let start = out.len();
        let res = self.encode_inner(out);
        if res.is_err() {
            out.truncate(start);
        }
        res
    }

    fn encode_inner(&self, out: &mut Vec<u8>) -> Result<()> {
        self.common.encode(out, false);
        out.put_u64(self.config_version);
        out.put_u16(self.backends.len() as u16);
        out.put_u8(u8::from(self.detached));
        out.put_u8(0);
        out.put_u16(self.authority.len() as u16);
        out.put_u16(self.policy_ver);
        out.put_u32(self.policy.len() as u32);
        out.put_zeros(4);
        out.extend_from_slice(self.authority.as_bytes());
        for b in &self.backends {
            if b.url.is_empty() || b.url.len() > MAX_URL_LEN || b.url.contains('\0') {
                return Err(FormatError::BadString {
                    what: WHAT,
                    field: "url",
                });
            }
            if b.id.is_empty() || b.id.len() > MAX_ID_LEN {
                return Err(FormatError::BadString {
                    what: WHAT,
                    field: "backend id",
                });
            }
            out.put_u16(b.id.len() as u16);
            out.put_u16(b.url.len() as u16);
            out.put_u8(b.role as u8);
            out.put_u8(b.state as u8);
            out.put_zeros(2);
            out.put_u64(b.since_seq.0);
            put_id(out, "backend id", &b.id, 1, MAX_ID_LEN)?;
            out.extend_from_slice(b.url.as_bytes());
        }
        out.extend_from_slice(&self.policy);
        Ok(())
    }

    /// Parses a whole body.
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut r = Reader::new(body, "commit config");
        let (common, _) = CommonHeader::decode(&mut r)?;
        common.expect(CommitKind::Config)?;
        let config_version = r.u64()?;
        let n_backends = r.u16()?;
        let detached = match r.u8()? {
            0 => false,
            1 => true,
            v => return Err(FormatError::range(WHAT, "detached", u64::from(v))),
        };
        r.reserved(1)?;
        let authority_len = usize::from(r.u16()?);
        let policy_ver = r.u16()?;
        let policy_len = r.u32()?;
        r.reserved(4)?;
        if authority_len > MAX_ID_LEN {
            return Err(FormatError::range(
                WHAT,
                "authority_len",
                authority_len as u64,
            ));
        }
        let authority = if authority_len == 0 {
            String::new()
        } else {
            read_id(&mut r, "authority", authority_len)?
        };
        // 16 fixed bytes plus at least one byte each of id and url.
        let n = r.check_count(
            "n_backends",
            u64::from(n_backends),
            u64::from(MAX_BACKENDS),
            18,
        )?;
        let mut backends = Vec::with_capacity(n);
        for _ in 0..n {
            let id_len = usize::from(r.u16()?);
            let url_len = usize::from(r.u16()?);
            let role = match r.u8()? {
                1 => BackendRole::Authority,
                2 => BackendRole::Mirror,
                v => return Err(FormatError::range(WHAT, "role", u64::from(v))),
            };
            let state = match r.u8()? {
                1 => BackendState::Joining,
                2 => BackendState::CatchingUp,
                3 => BackendState::InSync,
                4 => BackendState::Draining,
                5 => BackendState::Removed,
                v => return Err(FormatError::range(WHAT, "state", u64::from(v))),
            };
            r.reserved(2)?;
            let since_seq = CommitSeq(r.u64()?);
            if id_len == 0 || id_len > MAX_ID_LEN {
                return Err(FormatError::range(WHAT, "id_len", id_len as u64));
            }
            if url_len == 0 || url_len > MAX_URL_LEN {
                return Err(FormatError::range(WHAT, "url_len", url_len as u64));
            }
            let id = read_id(&mut r, "backend id", id_len)?;
            let url = r.string("url", url_len)?.to_owned();
            backends.push(Backend {
                id,
                url,
                role,
                state,
                since_seq,
            });
        }
        r.check_count(
            "policy_len",
            u64::from(policy_len),
            u64::from(MAX_POLICY),
            1,
        )?;
        let policy = r.take(policy_len as usize)?.to_vec();
        r.finish()?;
        let c = Self {
            common,
            config_version,
            detached,
            authority,
            policy_ver,
            backends,
            policy,
        };
        c.validate()?;
        Ok(c)
    }
}

/// Why authority moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransferReason {
    /// `set-authority`.
    Planned = 1,
    /// A steal.
    Steal = 2,
}

/// TRANSFER commit body (kind 5).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TransferCommit {
    /// Common header; `epoch` is the new epoch `e + 1`.
    pub common: CommonHeader,
    /// The previous `config_version` plus 1.
    pub config_version: u64,
    /// Why.
    pub reason: TransferReason,
    /// The new authority backend id.
    pub to: String,
}

impl TransferCommit {
    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.common.expect(CommitKind::Transfer)?;
        if self.config_version == 0 {
            return Err(FormatError::range(WHAT, "config_version", 0));
        }
        let start = out.len();
        self.common.encode(out, false);
        out.put_u64(self.config_version);
        out.put_u8(self.reason as u8);
        out.put_u8(0);
        out.put_u16(self.to.len() as u16);
        out.put_zeros(4);
        let res = put_id(out, "to", &self.to, 1, MAX_ID_LEN);
        if res.is_err() {
            out.truncate(start);
        }
        res
    }

    /// Parses a whole body.
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut r = Reader::new(body, "commit transfer");
        let (common, _) = CommonHeader::decode(&mut r)?;
        common.expect(CommitKind::Transfer)?;
        let config_version = r.u64()?;
        if config_version == 0 {
            return Err(FormatError::range(WHAT, "config_version", 0));
        }
        let reason = match r.u8()? {
            1 => TransferReason::Planned,
            2 => TransferReason::Steal,
            v => return Err(FormatError::range(WHAT, "reason", u64::from(v))),
        };
        r.reserved(1)?;
        let to_len = usize::from(r.u16()?);
        r.reserved(4)?;
        if to_len == 0 || to_len > MAX_ID_LEN {
            return Err(FormatError::range(WHAT, "to_len", to_len as u64));
        }
        let to = read_id(&mut r, "to", to_len)?;
        r.finish()?;
        Ok(Self {
            common,
            config_version,
            reason,
            to,
        })
    }
}

/// Any commit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Commit {
    /// Kind 1.
    Full(Full),
    /// Kind 2.
    Delta(Delta),
    /// Kind 3.
    Epoch(EpochCommit),
    /// Kind 4.
    Config(ConfigCommit),
    /// Kind 5.
    Transfer(TransferCommit),
}

impl Commit {
    /// The commit kind.
    pub fn kind(&self) -> CommitKind {
        self.common().kind
    }

    /// The common header.
    pub fn common(&self) -> &CommonHeader {
        match self {
            Self::Full(c) => &c.common,
            Self::Delta(c) => &c.common,
            Self::Epoch(c) => &c.common,
            Self::Config(c) => &c.common,
            Self::Transfer(c) => &c.common,
        }
    }

    /// The envelope kind this commit is stored under (1, or 3 for DELTA).
    pub fn object_kind(&self) -> ObjectKind {
        self.kind().object_kind()
    }

    /// Appends the body (the plaintext an encrypted volume seals).
    pub fn encode_body(&self, out: &mut Vec<u8>) -> Result<()> {
        match self {
            Self::Full(c) => c.encode(out),
            Self::Delta(c) => c.encode(out),
            Self::Epoch(c) => c.encode(out),
            Self::Config(c) => c.encode(out),
            Self::Transfer(c) => c.encode(out),
        }
    }

    /// Parses a body found under envelope kind `object_kind`; the commit kind
    /// and the envelope kind must agree (DELTA iff kind 3).
    pub fn decode_body(object_kind: ObjectKind, body: &[u8]) -> Result<Self> {
        let k = body.get(1).ok_or(FormatError::Truncated {
            what: WHAT,
            need: 2,
            have: body.len(),
        })?;
        let kind = CommitKind::from_u8(*k)?;
        if kind.object_kind() != object_kind {
            return Err(FormatError::invalid(
                WHAT,
                "envelope kind does not match commit_kind",
            ));
        }
        Ok(match kind {
            CommitKind::Full => Self::Full(Full::decode(body)?),
            CommitKind::Delta => Self::Delta(Delta::decode(body)?),
            CommitKind::Epoch => Self::Epoch(EpochCommit::decode(body)?),
            CommitKind::Config => Self::Config(ConfigCommit::decode(body)?),
            CommitKind::Transfer => Self::Transfer(TransferCommit::decode(body)?),
        })
    }

    /// A form 0 object: the body in an envelope with the caller's `mac`.
    pub fn encode_object_form0(&self, mac: &[u8; 32]) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.encode_body(&mut body)?;
        envelope::encode_form0(self.object_kind(), &body, mac)
    }

    /// The form 0 object without its `mac`: the bytes the MAC covers after
    /// the AAD.
    pub fn encode_object_form0_unsigned(&self) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.encode_body(&mut body)?;
        envelope::form0_unsigned(self.object_kind(), &body)
    }

    /// A form 1 object from the parts the caller sealed from
    /// [`Commit::encode_body`].
    pub fn encode_object_form1(
        &self,
        nonce: &[u8; 24],
        ciphertext: &[u8],
        tag: &[u8; 16],
    ) -> Result<Vec<u8>> {
        envelope::encode_form1(self.object_kind(), nonce, ciphertext, tag)
    }
}

/// Parses the envelope of a commit object (kind 1 or 3) of either form.
pub fn decode_envelope(buf: &[u8], form: Form) -> Result<Envelope<'_>> {
    Envelope::decode(
        buf,
        &[ObjectKind::Commit, ObjectKind::Delta],
        form,
        ObjectKind::Delta.max_body_len(),
    )
}

/// A form 0 commit object, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedObject<'a> {
    /// The envelope.
    pub envelope: Envelope<'a>,
    /// The `mac` the caller verifies over `meta_aad ‖ envelope.signed`.
    pub mac: [u8; 32],
    /// The commit.
    pub commit: Commit,
}

/// Parses a form 0 commit object. (Form 1 objects: open the envelope with
/// [`decode_envelope`], unseal, then [`Commit::decode_body`].)
pub fn decode_object(buf: &[u8]) -> Result<DecodedObject<'_>> {
    let env = decode_envelope(buf, Form::Mac)?;
    let EnvelopeBody::Mac { body, mac } = env.body.clone() else {
        return Err(FormatError::invalid(WHAT, "not a form 0 envelope"));
    };
    let commit = Commit::decode_body(env.kind, body)?;
    if commit.object_kind() != env.kind {
        return Err(FormatError::invalid(
            WHAT,
            "envelope kind does not match commit_kind",
        ));
    }
    Ok(DecodedObject {
        envelope: env,
        mac,
        commit,
    })
}

/// Chain rule between a commit and its parent (section 10.7): numbers are
/// contiguous, `parent_hash` is the hash of the parent object as stored (the
/// caller computes it), and the epoch does not decrease. `created_at` is a
/// writer rule and is not checked.
pub fn check_child(
    parent: &CommonHeader,
    parent_object_hash: &[u8; 32],
    child: &CommonHeader,
) -> Result<()> {
    if parent.commit_seq.checked_add(1)? != child.commit_seq {
        return Err(FormatError::invalid(WHAT, "commit_seq is not parent + 1"));
    }
    if &child.parent_hash != parent_object_hash {
        return Err(FormatError::invalid(
            WHAT,
            "parent_hash does not match the parent object",
        ));
    }
    if child.epoch < parent.epoch {
        return Err(FormatError::invalid(WHAT, "epoch decreased"));
    }
    Ok(())
}

/// Genesis rule (D5): `commit_seq` 0 and `parent_hash` the BLAKE3-256 of the
/// exact `volume` bytes (the caller computes it).
pub fn check_genesis(c: &CommonHeader, volume_hash: &[u8; 32]) -> Result<()> {
    if c.commit_seq != CommitSeq(0) {
        return Err(FormatError::invalid(WHAT, "genesis has commit_seq 0"));
    }
    if &c.parent_hash != volume_hash {
        return Err(FormatError::invalid(
            WHAT,
            "genesis parent_hash is not the volume hash",
        ));
    }
    Ok(())
}
