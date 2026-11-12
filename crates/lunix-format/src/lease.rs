//! Lease body (freeze item 10, on-disk-formats.md section 13). A metadata
//! envelope of kind 2 named `lease`; `epoch` is in the authenticated plaintext.

use crate::bytes::{Put, Reader};
use crate::envelope::{self, Envelope, EnvelopeBody};
use crate::error::{FormatError, Result};
use crate::ids::{Epoch, TimeMs};
use crate::mode::{Form, ObjectKind};
use crate::paths::{valid_backend_id, valid_host};

const WHAT: &str = "lease";

/// Lease body version.
pub const BODY_VER: u8 = 1;
/// Fixed part of the body.
pub const FIXED_LEN: usize = 24;
/// Longest cluster name.
pub const MAX_CLUSTER: usize = 255;
/// Longest host or backend id.
pub const MAX_ID: usize = 64;

/// The lease.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Lease {
    /// The owner released it.
    pub released: bool,
    /// Owner epoch; a lease below the newest verified epoch is rejected.
    pub epoch: Epoch,
    /// Expiry.
    pub deadline: TimeMs,
    /// Owner host, 1 ..= 64 of `[a-z0-9.-]`.
    pub host: String,
    /// Owner cluster, 0 ..= 255 bytes of UTF-8 without NUL; empty = standalone.
    pub cluster: String,
    /// Best-effort successor (ADR-24 step 6): a backend id.
    pub transferred_to: Option<String>,
}

impl Lease {
    /// Appends the body.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        if !valid_host(&self.host) {
            return Err(FormatError::BadString {
                what: WHAT,
                field: "host",
            });
        }
        if self.cluster.len() > MAX_CLUSTER || self.cluster.contains('\0') {
            return Err(FormatError::BadString {
                what: WHAT,
                field: "cluster",
            });
        }
        let to = self.transferred_to.as_deref().unwrap_or("");
        if self.transferred_to.is_some() && !valid_backend_id(to) {
            return Err(FormatError::BadString {
                what: WHAT,
                field: "transferred_to",
            });
        }
        out.put_u8(BODY_VER);
        out.put_u8(u8::from(self.released));
        out.put_u16(self.host.len() as u16);
        out.put_u16(self.cluster.len() as u16);
        out.put_u16(to.len() as u16);
        out.put_u64(self.epoch.0);
        out.put_u64(self.deadline.0);
        out.extend_from_slice(self.host.as_bytes());
        out.extend_from_slice(self.cluster.as_bytes());
        out.extend_from_slice(to.as_bytes());
        Ok(())
    }

    /// Parses a whole body.
    pub fn decode(b: &[u8]) -> Result<Self> {
        let mut r = Reader::new(b, WHAT);
        r.version_u8(BODY_VER)?;
        let released = match r.u8()? {
            0 => false,
            1 => true,
            v => return Err(FormatError::range(WHAT, "released", u64::from(v))),
        };
        let host_len = usize::from(r.u16()?);
        let cluster_len = usize::from(r.u16()?);
        let to_len = usize::from(r.u16()?);
        if host_len == 0 || host_len > MAX_ID {
            return Err(FormatError::range(WHAT, "host_len", host_len as u64));
        }
        if cluster_len > MAX_CLUSTER {
            return Err(FormatError::range(WHAT, "cluster_len", cluster_len as u64));
        }
        if to_len > MAX_ID {
            return Err(FormatError::range(
                WHAT,
                "transferred_to_len",
                to_len as u64,
            ));
        }
        let epoch = Epoch(r.u64()?);
        let deadline = TimeMs(r.u64()?);
        let host = r.string("host", host_len)?.to_owned();
        if !valid_host(&host) {
            return Err(FormatError::BadString {
                what: WHAT,
                field: "host",
            });
        }
        let cluster = r.string("cluster", cluster_len)?.to_owned();
        let transferred_to = if to_len == 0 {
            None
        } else {
            let s = r.string("transferred_to", to_len)?;
            if !valid_backend_id(s) {
                return Err(FormatError::BadString {
                    what: WHAT,
                    field: "transferred_to",
                });
            }
            Some(s.to_owned())
        };
        r.finish()?;
        Ok(Self {
            released,
            epoch,
            deadline,
            host,
            cluster,
            transferred_to,
        })
    }

    /// A form 0 object (kind 2) with the caller's `mac`.
    pub fn encode_object_form0(&self, mac: &[u8; 32]) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        self.encode(&mut body)?;
        envelope::encode_form0(ObjectKind::Lease, &body, mac)
    }

    /// Parses a form 0 object; returns the lease and the `mac` to verify.
    pub fn decode_object(buf: &[u8]) -> Result<(Self, [u8; 32])> {
        let env = Envelope::decode(
            buf,
            &[ObjectKind::Lease],
            Form::Mac,
            ObjectKind::Lease.max_body_len(),
        )?;
        let EnvelopeBody::Mac { body, mac } = env.body else {
            return Err(FormatError::invalid(WHAT, "not a form 0 envelope"));
        };
        Ok((Self::decode(body)?, mac))
    }
}
