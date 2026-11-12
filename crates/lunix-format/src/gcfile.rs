//! GC files (freeze item 15, on-disk-formats.md section 16): the
//! `gc/<pass:016x>` evidence object and the local `state/gc/condemned`
//! envelope.

use crate::bytes::{crc32c, Put, Reader};
use crate::envelope::{self, Envelope, EnvelopeBody};
use crate::error::{FormatError, Result};
use crate::mode::{Form, ObjectKind};
use crate::opaque::OpaqueBody;

const WHAT: &str = "gc pass";

/// The opaque-body `ver` of a phase 1 `gc/` object.
pub const GC_BODY_VER: u16 = 1;

/// Body of `gc/<pass:016x>`: an opaque body whose `ver` is 1 and whose bytes
/// are the pass's JSON lines (`docs/reference/gc.md`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GcPassBody {
    /// The pass's JSON lines.
    pub bytes: Vec<u8>,
}

impl GcPassBody {
    /// Appends the body with `ver` fixed to 1.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        OpaqueBody {
            ver: GC_BODY_VER,
            bytes: self.bytes.clone(),
        }
        .encode(out)
    }

    /// Parses a body; any `ver` but 1 is rejected.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let o = OpaqueBody::decode(b)?;
        if o.ver != GC_BODY_VER {
            return Err(FormatError::BadVersion {
                what: WHAT,
                got: u64::from(o.ver),
            });
        }
        Ok(Self { bytes: o.bytes })
    }

    /// A form 0 `gc/` object (kind 7) with the caller's `mac`.
    pub fn encode_object_form0(&self, mac: &[u8; 32]) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.encode(&mut body)?;
        envelope::encode_form0(ObjectKind::Gc, &body, mac)
    }

    /// Parses a form 0 `gc/` object; returns the body and the `mac`.
    pub fn decode_object(buf: &[u8]) -> Result<(Self, [u8; 32])> {
        let env = Envelope::decode(
            buf,
            &[ObjectKind::Gc],
            Form::Mac,
            ObjectKind::Gc.max_body_len(),
        )?;
        let EnvelopeBody::Mac { body, mac } = env.body else {
            return Err(FormatError::invalid(WHAT, "not a form 0 envelope"));
        };
        Ok((Self::decode(body)?, mac))
    }
}

/// Magic of `state/gc/condemned`.
pub const CONDEMNED_MAGIC: &[u8; 8] = b"LUNIXGCC";
/// Envelope version of `state/gc/condemned`.
pub const CONDEMNED_VER: u16 = 1;
/// Maximum body length.
pub const CONDEMNED_MAX_BODY: u32 = 256 << 20;

/// The `state/gc/condemned` file. An error from [`Condemned::decode`]
/// (unknown version, bad CRC, anything) means the list is lost and the caller
/// re-marks; nothing is repaired.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Condemned {
    /// Version of the body, defined by `docs/reference/gc.md`; at least 1.
    pub body_ver: u16,
    /// The condemned list.
    pub body: Vec<u8>,
}

impl Condemned {
    /// Appends the file.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        if self.body_ver == 0 {
            return Err(FormatError::range("condemned", "body_ver", 0));
        }
        if self.body.len() as u64 > u64::from(CONDEMNED_MAX_BODY) {
            return Err(FormatError::count(
                "condemned",
                "body_len",
                self.body.len() as u64,
                u64::from(CONDEMNED_MAX_BODY),
            ));
        }
        let start = out.len();
        out.extend_from_slice(CONDEMNED_MAGIC);
        out.put_u16(CONDEMNED_VER);
        out.put_u16(self.body_ver);
        out.put_u32(self.body.len() as u32);
        out.extend_from_slice(&self.body);
        let crc = crc32c(&out[start..]);
        out.put_u32(crc);
        Ok(())
    }

    /// Parses a whole file.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b, "condemned");
        r.magic(CONDEMNED_MAGIC)?;
        r.version_u16(CONDEMNED_VER)?;
        let body_ver = r.u16()?;
        if body_ver == 0 {
            return Err(FormatError::range("condemned", "body_ver", 0));
        }
        let len = r.u32()?;
        r.check_count("body_len", u64::from(len), u64::from(CONDEMNED_MAX_BODY), 1)?;
        let body = r.take(len as usize)?.to_vec();
        let covered = r.consumed().len();
        let crc = r.u32()?;
        r.finish()?;
        if crc32c(&b[..covered]) != crc {
            return Err(FormatError::Checksum { what: "condemned" });
        }
        Ok(Self { body_ver, body })
    }
}
