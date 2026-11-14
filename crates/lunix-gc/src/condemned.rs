//! The condemned list: the body of `state/gc/condemned` (body version 1,
//! `docs/reference/gc.md`). The envelope (magic, version, CRC) is
//! [`lunix_format::gcfile::Condemned`]; this module defines what is inside.
//!
//! The list is a **hint**: losing it (absent, unknown version, bad CRC, any
//! decode error) is safe, because every object in it is re-marked before it
//! is deleted, and the index's condemned flags are reconciled with it when it
//! is loaded.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use lunix_format::bytes::{Put, Reader};
use lunix_format::gcfile::{Condemned, CONDEMNED_MAX_BODY};
use lunix_format::{CommitSeq, FormatError, ObjId, TimeMs};

const WHAT: &str = "condemned list";

/// The body version this crate writes and reads.
pub const CONDEMNED_BODY_VER: u16 = 1;

/// Encoded length of one entry.
pub const ENTRY_LEN: usize = 25;

/// Most entries a list may hold (the envelope's body cap).
pub const MAX_ENTRIES: u32 = (CONDEMNED_MAX_BODY - 4) / ENTRY_LEN as u32;

/// An object GC can delete.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ObjectRef {
    /// A sealed segment (`segments/<id>`).
    Segment(ObjId),
    /// A map pack (`map/<id>`).
    Pack(ObjId),
    /// An index run (`index/<id>`).
    Run(ObjId),
    /// A commit object below `chain_floor` (`state/commits/<seq>`).
    Commit(CommitSeq),
}

impl ObjectRef {
    /// The kind byte of the encoding.
    pub fn kind_byte(self) -> u8 {
        match self {
            Self::Segment(_) => 1,
            Self::Pack(_) => 2,
            Self::Run(_) => 3,
            Self::Commit(_) => 4,
        }
    }

    /// The kind as the evidence lines name it.
    pub fn kind_name(self) -> &'static str {
        match self {
            Self::Segment(_) => "segment",
            Self::Pack(_) => "pack",
            Self::Run(_) => "run",
            Self::Commit(_) => "commit",
        }
    }

    /// The raw id (an object id, or a `commit_seq`).
    pub fn raw(self) -> u64 {
        match self {
            Self::Segment(id) | Self::Pack(id) | Self::Run(id) => id.raw(),
            Self::Commit(seq) => seq.0,
        }
    }

    fn from_parts(kind: u8, raw: u64) -> lunix_format::Result<Self> {
        let obj = || ObjId::from_raw(raw);
        Ok(match kind {
            1 => Self::Segment(obj()?),
            2 => Self::Pack(obj()?),
            3 => Self::Run(obj()?),
            4 => Self::Commit(CommitSeq(raw)),
            _ => return Err(FormatError::range(WHAT, "kind", u64::from(kind))),
        })
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:016x}", self.kind_name(), self.raw())
    }
}

/// One condemned object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CondemnedEntry {
    /// Wall-clock time of the condemn (informational: the grace is measured
    /// on the monotonic clock of the process, see [`CondemnedList`]).
    pub condemned_at: TimeMs,
    /// The object's size when it was condemned.
    pub bytes: u64,
}

/// The decoded list: one entry per object, ordered by `(kind, id)`.
///
/// The grace of an entry runs on the collector's monotonic clock from the
/// moment it was condemned **in this process**; entries loaded from disk
/// start their grace again at load time (a restart never shortens a grace).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CondemnedList {
    /// The entries.
    pub entries: BTreeMap<ObjectRef, CondemnedEntry>,
}

impl CondemnedList {
    /// Encodes the body (body version 1).
    ///
    /// # Errors
    /// `Count` beyond [`MAX_ENTRIES`].
    pub fn encode_body(&self, out: &mut Vec<u8>) -> lunix_format::Result<()> {
        let n = u32::try_from(self.entries.len())
            .ok()
            .filter(|&n| n <= MAX_ENTRIES)
            .ok_or(FormatError::count(
                WHAT,
                "count",
                self.entries.len() as u64,
                u64::from(MAX_ENTRIES),
            ))?;
        out.reserve(4 + ENTRY_LEN * self.entries.len());
        out.put_u32(n);
        for (obj, e) in &self.entries {
            out.put_u8(obj.kind_byte());
            out.put_u64(obj.raw());
            out.put_u64(e.condemned_at.0);
            out.put_u64(e.bytes);
        }
        Ok(())
    }

    /// Decodes a body of version 1: entries strictly ascending by
    /// `(kind, id)`, nothing after the last.
    ///
    /// # Errors
    /// Any structural fault; the caller treats the list as lost.
    pub fn decode_body(b: &[u8]) -> lunix_format::Result<Self> {
        let mut r = Reader::new(b, WHAT);
        let count = u64::from(r.u32()?);
        let n = r.check_count("count", count, u64::from(MAX_ENTRIES), ENTRY_LEN)?;
        let mut entries = BTreeMap::new();
        let mut last: Option<ObjectRef> = None;
        for _ in 0..n {
            let obj = ObjectRef::from_parts(r.u8()?, r.u64()?)?;
            if last.is_some_and(|l| l >= obj) {
                return Err(FormatError::not_canonical(
                    WHAT,
                    "entries are not strictly ascending",
                ));
            }
            last = Some(obj);
            let condemned_at = TimeMs(r.u64()?);
            let bytes = r.u64()?;
            entries.insert(
                obj,
                CondemnedEntry {
                    condemned_at,
                    bytes,
                },
            );
        }
        r.finish()?;
        Ok(Self { entries })
    }

    /// Wraps the body in the `state/gc/condemned` envelope.
    ///
    /// # Errors
    /// As [`encode_body`](Self::encode_body).
    pub fn to_file(&self) -> lunix_format::Result<Condemned> {
        let mut body = Vec::new();
        self.encode_body(&mut body)?;
        Ok(Condemned {
            body_ver: CONDEMNED_BODY_VER,
            body,
        })
    }

    /// Reads the list from its envelope; `None` for another body version or a
    /// body that does not decode (the list is then lost: re-mark).
    pub fn from_file(file: &Condemned) -> Option<Self> {
        if file.body_ver != CONDEMNED_BODY_VER {
            return None;
        }
        Self::decode_body(&file.body).ok()
    }
}

/// When each entry's grace started, on the collector's monotonic clock.
pub(crate) type GraceStarts = BTreeMap<ObjectRef, Duration>;
