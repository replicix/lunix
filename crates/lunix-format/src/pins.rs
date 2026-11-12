//! Pin bodies: `snapshots/`, `readers/`, `holds/` (freeze item 7,
//! on-disk-formats.md section 14). Each is the body of a metadata envelope of
//! kind 10, 8 and 9. Maximum body 4 KiB.

use crate::bytes::{Put, Reader};
use crate::envelope::{self, Envelope, EnvelopeBody};
use crate::error::{FormatError, Result};
use crate::ids::{CommitSeq, TimeMs};
use crate::mode::{Form, ObjectKind};

/// Pin body version.
pub const BODY_VER: u8 = 1;
/// Longest snapshot label.
pub const MAX_LABEL: usize = 1024;

fn object_decode(buf: &[u8], kind: ObjectKind) -> Result<(&[u8], [u8; 32])> {
    let env = Envelope::decode(buf, &[kind], Form::Mac, kind.max_body_len())?;
    match env.body {
        EnvelopeBody::Mac { body, mac } => Ok((body, mac)),
        EnvelopeBody::Sealed { .. } => Err(FormatError::invalid("pin", "not a form 0 envelope")),
    }
}

/// How consistent a snapshot is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Consistency {
    /// Crash-consistent.
    Crash = 1,
    /// File system frozen.
    FsFrozen = 2,
}

/// `snapshots/<name>` body.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SnapshotPin {
    /// Consistency of the snapshot.
    pub consistency: Consistency,
    /// The FULL commit it pins.
    pub commit_seq: CommitSeq,
    /// Creation time.
    pub created: TimeMs,
    /// Label, at most 1024 bytes of UTF-8 without NUL.
    pub label: String,
}

impl SnapshotPin {
    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        if self.label.len() > MAX_LABEL || self.label.contains('\0') {
            return Err(FormatError::BadString {
                what: "snapshot pin",
                field: "label",
            });
        }
        out.put_u8(BODY_VER);
        out.put_u8(self.consistency as u8);
        out.put_u16(self.label.len() as u16);
        out.put_zeros(4);
        out.put_u64(self.commit_seq.0);
        out.put_u64(self.created.0);
        out.extend_from_slice(self.label.as_bytes());
        Ok(())
    }

    /// Parses a whole body.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b, "snapshot pin");
        r.version_u8(BODY_VER)?;
        let consistency = match r.u8()? {
            1 => Consistency::Crash,
            2 => Consistency::FsFrozen,
            v => {
                return Err(FormatError::range(
                    "snapshot pin",
                    "consistency",
                    u64::from(v),
                ))
            }
        };
        let n = usize::from(r.u16()?);
        if n > MAX_LABEL {
            return Err(FormatError::count(
                "snapshot pin",
                "label_len",
                n as u64,
                MAX_LABEL as u64,
            ));
        }
        r.reserved(4)?;
        let commit_seq = CommitSeq(r.u64()?);
        let created = TimeMs(r.u64()?);
        let label = r.string("label", n)?.to_owned();
        r.finish()?;
        Ok(Self {
            consistency,
            commit_seq,
            created,
            label,
        })
    }

    /// A form 0 object (kind 10) with the caller's `mac`.
    pub fn encode_object_form0(&self, mac: &[u8; 32]) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.encode(&mut body)?;
        envelope::encode_form0(ObjectKind::Snapshots, &body, mac)
    }

    /// Parses a form 0 object; returns the pin and the `mac`.
    pub fn decode_object(buf: &[u8]) -> Result<(Self, [u8; 32])> {
        let (body, mac) = object_decode(buf, ObjectKind::Snapshots)?;
        Ok((Self::decode(body)?, mac))
    }
}

/// `readers/<host>-<id>` body (24 bytes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReaderPin {
    /// The commit the reader holds.
    pub commit_seq: CommitSeq,
    /// When the pin lapses.
    pub deadline: TimeMs,
}

impl ReaderPin {
    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.put_u8(BODY_VER);
        out.put_zeros(7);
        out.put_u64(self.commit_seq.0);
        out.put_u64(self.deadline.0);
    }

    /// Parses a whole body.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b, "reader pin");
        r.version_u8(BODY_VER)?;
        r.reserved(7)?;
        let commit_seq = CommitSeq(r.u64()?);
        let deadline = TimeMs(r.u64()?);
        r.finish()?;
        Ok(Self {
            commit_seq,
            deadline,
        })
    }

    /// A form 0 object (kind 8) with the caller's `mac`.
    pub fn encode_object_form0(&self, mac: &[u8; 32]) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.encode(&mut body);
        envelope::encode_form0(ObjectKind::Readers, &body, mac)
    }

    /// Parses a form 0 object; returns the pin and the `mac`.
    pub fn decode_object(buf: &[u8]) -> Result<(Self, [u8; 32])> {
        let (body, mac) = object_decode(buf, ObjectKind::Readers)?;
        Ok((Self::decode(body)?, mac))
    }
}

/// `holds/<child-uuid>` body (16 bytes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HoldPin {
    /// The base commit a child volume depends on.
    pub base_seq: CommitSeq,
}

impl HoldPin {
    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.put_u8(BODY_VER);
        out.put_zeros(7);
        out.put_u64(self.base_seq.0);
    }

    /// Parses a whole body.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b, "hold pin");
        r.version_u8(BODY_VER)?;
        r.reserved(7)?;
        let base_seq = CommitSeq(r.u64()?);
        r.finish()?;
        Ok(Self { base_seq })
    }

    /// A form 0 object (kind 9) with the caller's `mac`.
    pub fn encode_object_form0(&self, mac: &[u8; 32]) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.encode(&mut body);
        envelope::encode_form0(ObjectKind::Holds, &body, mac)
    }

    /// Parses a form 0 object; returns the pin and the `mac`.
    pub fn decode_object(buf: &[u8]) -> Result<(Self, [u8; 32])> {
        let (body, mac) = object_decode(buf, ObjectKind::Holds)?;
        Ok((Self::decode(body)?, mac))
    }
}
