//! Metadata envelope and metadata AAD (freeze item 7, on-disk-formats.md
//! sections 4.4 and 9).
//!
//! The envelope wraps every metadata object. This crate frames and parses it;
//! computing or verifying the MAC and sealing or opening the body are
//! `lunix-crypto`'s, which take [`Envelope::signed`], [`mac_input`] and
//! [`meta_aad`] from here.

use crate::bytes::{Put, Reader};
use crate::error::{FormatError, Result};
use crate::mode::{Form, ObjectKind};

const WHAT: &str = "envelope";

/// Envelope magic.
pub const MAGIC: &[u8; 4] = b"LXM1";
/// Envelope version.
pub const ENV_VER: u8 = 1;
/// Fixed header length (magic, version, kind, form, reserved, `body_len`).
pub const HEADER_LEN: usize = 12;
/// Form 0 `mac` length.
pub const MAC_LEN: usize = 32;
/// Form 1 nonce length.
pub const NONCE_LEN: usize = 24;
/// Form 1 tag length.
pub const TAG_LEN: usize = 16;
/// Bytes a form 0 envelope adds to its body.
pub const FORM0_OVERHEAD: usize = HEADER_LEN + MAC_LEN;
/// Bytes a form 1 envelope adds to its body.
pub const FORM1_OVERHEAD: usize = HEADER_LEN + NONCE_LEN + TAG_LEN;
/// `"lunix meta v1\0"`.
pub const META_AAD_LABEL: &[u8; 14] = b"lunix meta v1\0";
/// Longest bucket-relative object name in an AAD.
pub const MAX_NAME_LEN: usize = 1024;

/// Builds the metadata AAD
/// `"lunix meta v1\0" ‖ id(16) ‖ kind ‖ name_len(u32) ‖ name`.
///
/// `id` is the volume uuid, or the family id for the keyring. `name` is the
/// bucket-relative object name (D9), never a local path.
pub fn meta_aad(id: &[u8; 16], kind: ObjectKind, name: &str) -> Result<Vec<u8>> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(FormatError::range(
            "meta aad",
            "name_len",
            name.len() as u64,
        ));
    }
    let mut v = Vec::with_capacity(14 + 16 + 1 + 4 + name.len());
    v.extend_from_slice(META_AAD_LABEL);
    v.extend_from_slice(id);
    v.put_u8(kind.id());
    v.put_u32(name.len() as u32);
    v.extend_from_slice(name.as_bytes());
    Ok(v)
}

/// The form 0 MAC input: `meta_aad ‖ envelope[0 .. 12 + body_len]`.
pub fn mac_input(aad: &[u8], signed: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(aad.len() + signed.len());
    v.extend_from_slice(aad);
    v.extend_from_slice(signed);
    v
}

/// A parsed envelope, borrowing the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope<'a> {
    /// The object kind byte.
    pub kind: ObjectKind,
    /// Plaintext body length.
    pub body_len: u32,
    /// The body variant.
    pub body: EnvelopeBody<'a>,
    /// Form 0: `envelope[0 .. 12 + body_len]`, the bytes the MAC covers after
    /// the AAD. Form 1: the same prefix (checked, not authenticated).
    pub signed: &'a [u8],
}

/// Form-specific part of an envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeBody<'a> {
    /// Plain body and its MAC (or checksum, in plain mode).
    Mac {
        /// The body.
        body: &'a [u8],
        /// The 32-byte `mac`.
        mac: [u8; 32],
    },
    /// A sealed body.
    Sealed {
        /// XChaCha20-Poly1305 nonce.
        nonce: [u8; 24],
        /// Ciphertext (`body_len` bytes).
        ciphertext: &'a [u8],
        /// Poly1305 tag.
        tag: [u8; 16],
    },
}

impl<'a> Envelope<'a> {
    /// The form of this envelope.
    pub fn form(&self) -> Form {
        match self.body {
            EnvelopeBody::Mac { .. } => Form::Mac,
            EnvelopeBody::Sealed { .. } => Form::Sealed,
        }
    }

    /// Parses an envelope.
    ///
    /// `kinds` are the kinds the object's name allows (1 or 3 for `commits/`);
    /// `form` is the form the descriptor's mode requires; `max_body_len` is
    /// the kind's maximum ([`ObjectKind::max_body_len`], or a tighter bound).
    /// The whole input must be one envelope.
    pub fn decode(
        buf: &'a [u8],
        kinds: &[ObjectKind],
        form: Form,
        max_body_len: u32,
    ) -> Result<Self> {
        let mut r = Reader::new(buf, WHAT);
        r.magic(MAGIC)?;
        r.version_u8(ENV_VER)?;
        let kind = ObjectKind::from_u8(r.u8()?)?;
        if !kinds.contains(&kind) {
            return Err(FormatError::invalid(WHAT, "kind does not match the object"));
        }
        let got_form = Form::from_u8(r.u8()?)?;
        if got_form != form {
            return Err(FormatError::invalid(
                WHAT,
                "form does not match the volume mode",
            ));
        }
        r.reserved(1)?;
        let body_len = r.u32()?;
        if body_len > max_body_len {
            return Err(FormatError::count(
                WHAT,
                "body_len",
                u64::from(body_len),
                u64::from(max_body_len),
            ));
        }
        let n = body_len as usize;
        let signed_end = HEADER_LEN + n;
        let body = match form {
            Form::Mac => {
                let body = r.take(n)?;
                let mac = r.array::<MAC_LEN>()?;
                EnvelopeBody::Mac { body, mac }
            }
            Form::Sealed => {
                let nonce = r.array::<NONCE_LEN>()?;
                let ciphertext = r.take(n)?;
                let tag = r.array::<TAG_LEN>()?;
                EnvelopeBody::Sealed {
                    nonce,
                    ciphertext,
                    tag,
                }
            }
        };
        let signed = match form {
            // Form 0: header and body are contiguous.
            Form::Mac => &buf[..signed_end],
            // Form 1: only the header is outside the AEAD; expose it alone.
            Form::Sealed => &buf[..HEADER_LEN],
        };
        r.finish()?;
        Ok(Self {
            kind,
            body_len,
            body,
            signed,
        })
    }
}

/// Encodes a form 0 envelope around `body` with the given `mac`.
pub fn encode_form0(kind: ObjectKind, body: &[u8], mac: &[u8; 32]) -> Result<Vec<u8>> {
    let mut v = form0_unsigned(kind, body)?;
    v.extend_from_slice(mac);
    Ok(v)
}

/// The form 0 envelope without its `mac`: the bytes the MAC covers after the
/// AAD (`envelope[0 .. 12 + body_len]`).
pub fn form0_unsigned(kind: ObjectKind, body: &[u8]) -> Result<Vec<u8>> {
    let mut v = header(kind, Form::Mac, body.len())?;
    v.reserve(body.len() + MAC_LEN);
    v.extend_from_slice(body);
    Ok(v)
}

/// Encodes a form 1 envelope from the sealed parts. `ciphertext` is as long
/// as the plaintext body.
pub fn encode_form1(
    kind: ObjectKind,
    nonce: &[u8; 24],
    ciphertext: &[u8],
    tag: &[u8; 16],
) -> Result<Vec<u8>> {
    let mut v = header(kind, Form::Sealed, ciphertext.len())?;
    v.reserve(NONCE_LEN + ciphertext.len() + TAG_LEN);
    v.extend_from_slice(nonce);
    v.extend_from_slice(ciphertext);
    v.extend_from_slice(tag);
    Ok(v)
}

fn header(kind: ObjectKind, form: Form, body_len: usize) -> Result<Vec<u8>> {
    let max = kind.max_body_len();
    let n = u32::try_from(body_len)
        .ok()
        .filter(|&n| n <= max)
        .ok_or(FormatError::count(
            WHAT,
            "body_len",
            body_len as u64,
            u64::from(max),
        ))?;
    let mut v = Vec::with_capacity(HEADER_LEN + body_len + MAC_LEN);
    v.extend_from_slice(MAGIC);
    v.put_u8(ENV_VER);
    v.put_u8(kind.id());
    v.put_u8(form as u8);
    v.put_u8(0);
    v.put_u32(n);
    Ok(v)
}
