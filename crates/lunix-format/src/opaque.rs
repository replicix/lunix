//! Opaque envelope bodies of `heat/`, `dicts/`, `gc/space/` and `gc/`
//! (freeze item 7, on-disk-formats.md section 15, D6): only `ver` and the
//! length are frozen; `bytes` belongs to the phase that writes the kind.

use crate::bytes::{Put, Reader};
use crate::envelope::{self, Envelope, EnvelopeBody};
use crate::error::{FormatError, Result};
use crate::mode::{Form, ObjectKind};

const WHAT: &str = "opaque body";

/// Maximum `len`.
pub const MAX_LEN: u32 = 64 << 20;
/// Fixed part of the body.
pub const HEADER_LEN: usize = 8;

/// `{ver: u16, bytes}`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpaqueBody {
    /// Defined by the writer of the kind; at least 1.
    pub ver: u16,
    /// The writer's bytes, at most 64 MiB.
    pub bytes: Vec<u8>,
}

/// True for the kinds whose body is opaque.
pub fn is_opaque_kind(k: ObjectKind) -> bool {
    matches!(
        k,
        ObjectKind::Heat | ObjectKind::Dicts | ObjectKind::Space | ObjectKind::Gc
    )
}

impl OpaqueBody {
    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        if self.ver == 0 {
            return Err(FormatError::range(WHAT, "ver", 0));
        }
        if self.bytes.len() as u64 > u64::from(MAX_LEN) {
            return Err(FormatError::count(
                WHAT,
                "len",
                self.bytes.len() as u64,
                u64::from(MAX_LEN),
            ));
        }
        out.put_u16(self.ver);
        out.put_zeros(2);
        out.put_u32(self.bytes.len() as u32);
        out.extend_from_slice(&self.bytes);
        Ok(())
    }

    /// Parses a whole body.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b, WHAT);
        let ver = r.u16()?;
        if ver == 0 {
            return Err(FormatError::range(WHAT, "ver", 0));
        }
        r.reserved(2)?;
        let len = r.u32()?;
        r.check_count("len", u64::from(len), u64::from(MAX_LEN), 1)?;
        let bytes = r.take(len as usize)?.to_vec();
        r.finish()?;
        Ok(Self { ver, bytes })
    }

    /// A form 0 object of opaque kind `kind` with the caller's `mac`.
    pub fn encode_object_form0(&self, kind: ObjectKind, mac: &[u8; 32]) -> Result<Vec<u8>> {
        check_kind(kind)?;
        let mut body = Vec::new();
        self.encode(&mut body)?;
        envelope::encode_form0(kind, &body, mac)
    }

    /// Parses a form 0 object of opaque kind `kind`; returns the body and the
    /// `mac` to verify over `meta_aad ‖ envelope.signed`.
    pub fn decode_object(buf: &[u8], kind: ObjectKind) -> Result<(Self, [u8; 32])> {
        check_kind(kind)?;
        let env = Envelope::decode(buf, &[kind], Form::Mac, kind.max_body_len())?;
        let EnvelopeBody::Mac { body, mac } = env.body else {
            return Err(FormatError::invalid(WHAT, "not a form 0 envelope"));
        };
        Ok((Self::decode(body)?, mac))
    }
}

fn check_kind(kind: ObjectKind) -> Result<()> {
    if is_opaque_kind(kind) {
        Ok(())
    } else {
        Err(FormatError::invalid(WHAT, "kind has no opaque body"))
    }
}
