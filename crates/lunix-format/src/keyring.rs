//! Keyring (freeze item 9, on-disk-formats.md section 12).
//!
//! `families/<family-id>/keyring` is the one mutable key object of a family.
//! Unwrapping a slot and computing the MAC are `lunix-crypto`'s; this module
//! parses and builds the bytes, the slot AAD and the MAC input.

use crate::bytes::{Put, Reader};
use crate::envelope::meta_aad;
use crate::error::{FormatError, Result};
use crate::ids::{hex_lower, DekGen, FamilyId};
use crate::mode::ObjectKind;

const WHAT: &str = "keyring";

/// Keyring magic.
pub const MAGIC: &[u8; 8] = b"LUNIXKRG";
/// Keyring version.
pub const VER: u16 = 1;
/// Fixed header length.
pub const FIXED_LEN: usize = 48;
/// MAC length.
pub const MAC_LEN: usize = 32;
/// Most slots.
pub const MAX_SLOTS: u8 = 32;
/// Largest whole keyring.
pub const MAX_LEN: usize = 192 * 1024;
/// Passphrase slot `params_len`.
pub const PASSPHRASE_PARAMS_LEN: usize = 28;
/// `wrap_len` of kinds 1 and 2.
pub const KEY_WRAP_LEN: usize = 73;
/// Largest KMS `key_ref`.
pub const MAX_KEY_REF: usize = 1024;
/// Largest KMS wrap.
pub const MAX_KMS_WRAP: usize = 4096;
/// `"lunix slot v1\0"`.
pub const SLOT_AAD_LABEL: &[u8; 14] = b"lunix slot v1\0";
/// The only `wrap_ver` of v1: XChaCha20-Poly1305.
pub const WRAP_VER_XCHACHA: u8 = 1;

/// Argon2id read bound: largest memory (1 GiB).
pub const ARGON_READ_MAX_M_KIB: u32 = 1_048_576;
/// Argon2id read bound: largest `t`.
pub const ARGON_READ_MAX_T: u32 = 10;
/// Argon2id read bound: largest `p`.
pub const ARGON_READ_MAX_P: u32 = 16;
/// Argon2id write bound: smallest memory (19 MiB).
pub const ARGON_WRITE_MIN_M_KIB: u32 = 19_456;
/// Argon2id write bound: smallest `t`.
pub const ARGON_WRITE_MIN_T: u32 = 2;

/// What the family root is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RootKind {
    /// Key master key of an encrypted family.
    Kmk = 1,
    /// Integrity key of an integrity family.
    Ik = 2,
}

/// Argon2id parameters of a passphrase slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Argon2Params {
    /// Memory in KiB.
    pub m_kib: u32,
    /// Iterations.
    pub t: u32,
    /// Lanes.
    pub p: u32,
    /// Salt.
    pub salt: [u8; 16],
}

impl Argon2Params {
    /// Checks the bounds a reader accepts (stops a hostile keyring from
    /// demanding gigabytes of memory).
    pub fn check_read_bounds(&self) -> Result<()> {
        let ok = self.m_kib <= ARGON_READ_MAX_M_KIB
            && (1..=ARGON_READ_MAX_T).contains(&self.t)
            && (1..=ARGON_READ_MAX_P).contains(&self.p)
            && u64::from(self.m_kib) >= 8 * u64::from(self.p);
        if ok {
            Ok(())
        } else {
            Err(FormatError::invalid(
                WHAT,
                "argon2id parameters out of read bounds",
            ))
        }
    }

    /// Checks the bounds a writer must meet for new slots.
    pub fn check_write_bounds(&self) -> Result<()> {
        self.check_read_bounds()?;
        if self.m_kib < ARGON_WRITE_MIN_M_KIB || self.t < ARGON_WRITE_MIN_T {
            return Err(FormatError::invalid(
                WHAT,
                "argon2id parameters below write bounds",
            ));
        }
        Ok(())
    }
}

/// Slot kind with its parameters.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SlotParams {
    /// Kind 1: passphrase through Argon2id.
    Passphrase(Argon2Params),
    /// Kind 2: a key file of 32 bytes.
    KeyFile,
    /// Kind 3: a KMS key reference (UTF-8, 1 ..= 1024 bytes).
    Kms {
        /// The key reference.
        key_ref: String,
    },
}

impl SlotParams {
    /// The kind byte.
    pub fn kind(&self) -> u8 {
        match self {
            Self::Passphrase(_) => 1,
            Self::KeyFile => 2,
            Self::Kms { .. } => 3,
        }
    }
}

/// The wrapped root of a slot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SlotWrap {
    /// Kinds 1 and 2: `wrap_ver ‖ nonce ‖ ciphertext(32) ‖ tag`.
    Key {
        /// 1 = XChaCha20-Poly1305.
        wrap_ver: u8,
        /// Nonce.
        nonce: [u8; 24],
        /// Encrypted 32-byte root.
        ciphertext: [u8; 32],
        /// Poly1305 tag.
        tag: [u8; 16],
    },
    /// Kind 3: the KMS ciphertext (1 ..= 4096 bytes).
    Kms(Vec<u8>),
}

/// One key slot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Slot {
    /// At least 1; never reused.
    pub slot_id: u32,
    /// Kind and parameters.
    pub params: SlotParams,
    /// The wrapped root.
    pub wrap: SlotWrap,
}

impl Slot {
    fn validate(&self) -> Result<()> {
        if self.slot_id == 0 {
            return Err(FormatError::range(WHAT, "slot_id", 0));
        }
        match (&self.params, &self.wrap) {
            (SlotParams::Passphrase(a), SlotWrap::Key { wrap_ver, .. }) => {
                a.check_read_bounds()?;
                check_wrap_ver(*wrap_ver)
            }
            (SlotParams::KeyFile, SlotWrap::Key { wrap_ver, .. }) => check_wrap_ver(*wrap_ver),
            (SlotParams::Kms { key_ref }, SlotWrap::Kms(w)) => {
                if key_ref.is_empty() || key_ref.len() > MAX_KEY_REF || key_ref.contains('\0') {
                    return Err(FormatError::BadString {
                        what: WHAT,
                        field: "key_ref",
                    });
                }
                if w.is_empty() || w.len() > MAX_KMS_WRAP {
                    return Err(FormatError::range(WHAT, "wrap_len", w.len() as u64));
                }
                Ok(())
            }
            _ => Err(FormatError::invalid(WHAT, "slot kind and wrap disagree")),
        }
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        let mut params = Vec::new();
        match &self.params {
            SlotParams::Passphrase(a) => {
                params.put_u32(a.m_kib);
                params.put_u32(a.t);
                params.put_u32(a.p);
                params.extend_from_slice(&a.salt);
            }
            SlotParams::KeyFile => {}
            SlotParams::Kms { key_ref } => params.extend_from_slice(key_ref.as_bytes()),
        }
        let mut wrap = Vec::new();
        match &self.wrap {
            SlotWrap::Key {
                wrap_ver,
                nonce,
                ciphertext,
                tag,
            } => {
                wrap.put_u8(*wrap_ver);
                wrap.extend_from_slice(nonce);
                wrap.extend_from_slice(ciphertext);
                wrap.extend_from_slice(tag);
            }
            SlotWrap::Kms(w) => wrap.extend_from_slice(w),
        }
        out.put_u32(self.slot_id);
        out.put_u8(self.params.kind());
        out.put_u8(0);
        out.put_u16(params.len() as u16);
        out.put_u16(wrap.len() as u16);
        out.put_zeros(2);
        out.extend_from_slice(&params);
        out.extend_from_slice(&wrap);
        Ok(())
    }

    fn read(r: &mut Reader<'_>) -> Result<Self> {
        let slot_id = r.u32()?;
        let kind = r.u8()?;
        r.reserved(1)?;
        let params_len = usize::from(r.u16()?);
        let wrap_len = usize::from(r.u16()?);
        r.reserved(2)?;
        let (params, wrap) = match kind {
            1 | 2 => {
                let want = if kind == 1 { PASSPHRASE_PARAMS_LEN } else { 0 };
                if params_len != want {
                    return Err(FormatError::range(WHAT, "params_len", params_len as u64));
                }
                if wrap_len != KEY_WRAP_LEN {
                    return Err(FormatError::range(WHAT, "wrap_len", wrap_len as u64));
                }
                let params = if kind == 1 {
                    SlotParams::Passphrase(Argon2Params {
                        m_kib: r.u32()?,
                        t: r.u32()?,
                        p: r.u32()?,
                        salt: r.array()?,
                    })
                } else {
                    SlotParams::KeyFile
                };
                let wrap = SlotWrap::Key {
                    wrap_ver: r.u8()?,
                    nonce: r.array()?,
                    ciphertext: r.array()?,
                    tag: r.array()?,
                };
                (params, wrap)
            }
            3 => {
                if params_len == 0 || params_len > MAX_KEY_REF {
                    return Err(FormatError::range(WHAT, "params_len", params_len as u64));
                }
                if wrap_len == 0 || wrap_len > MAX_KMS_WRAP {
                    return Err(FormatError::range(WHAT, "wrap_len", wrap_len as u64));
                }
                let key_ref = r.string("key_ref", params_len)?.to_owned();
                let wrap = r.take(wrap_len)?.to_vec();
                (SlotParams::Kms { key_ref }, SlotWrap::Kms(wrap))
            }
            // Kind 4 (PBKDF2, FIPS) is reserved and not defined in v1.
            _ => return Err(FormatError::range(WHAT, "slot kind", u64::from(kind))),
        };
        let s = Self {
            slot_id,
            params,
            wrap,
        };
        s.validate()?;
        Ok(s)
    }

    /// The slot AAD:
    /// `"lunix slot v1\0" ‖ family_id ‖ slot_id u32 ‖ kind u8 ‖ params`, where
    /// params are `m_kib, t, p` as u32 and the salt (kind 1), nothing (kind
    /// 2) or the `key_ref` length as u32 and its bytes (kind 3).
    pub fn aad(&self, family: &FamilyId) -> Vec<u8> {
        let mut v = Vec::with_capacity(14 + 16 + 5 + 28);
        v.extend_from_slice(SLOT_AAD_LABEL);
        v.extend_from_slice(&family.0);
        v.put_u32(self.slot_id);
        v.put_u8(self.params.kind());
        match &self.params {
            SlotParams::Passphrase(a) => {
                v.put_u32(a.m_kib);
                v.put_u32(a.t);
                v.put_u32(a.p);
                v.extend_from_slice(&a.salt);
            }
            SlotParams::KeyFile => {}
            SlotParams::Kms { key_ref } => {
                v.put_u32(key_ref.len() as u32);
                v.extend_from_slice(key_ref.as_bytes());
            }
        }
        v
    }
}

/// The keyring object.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Keyring {
    /// What the slots wrap.
    pub root_kind: RootKind,
    /// Current DEK generation (0 for an IK keyring).
    pub dek_gen: DekGen,
    /// The family (equals the path's).
    pub family_id: FamilyId,
    /// At least 1; rises on every change.
    pub keyring_seq: u64,
    /// Greater than every `slot_id`.
    pub next_slot_id: u32,
    /// 1 ..= 32 slots, sorted by `slot_id`, unique.
    pub slots: Vec<Slot>,
    /// HMAC-SHA-256 under the keyring MAC key.
    pub mac: [u8; 32],
}

impl Keyring {
    fn validate(&self) -> Result<()> {
        if self.slots.is_empty() || self.slots.len() > usize::from(MAX_SLOTS) {
            return Err(FormatError::count(
                WHAT,
                "slots",
                self.slots.len() as u64,
                u64::from(MAX_SLOTS),
            ));
        }
        if self.root_kind == RootKind::Ik && self.dek_gen.0 != 0 {
            return Err(FormatError::invalid(WHAT, "an IK keyring has dek_gen 0"));
        }
        if self.keyring_seq == 0 {
            return Err(FormatError::range(WHAT, "keyring_seq", 0));
        }
        let mut prev = 0u32;
        for s in &self.slots {
            if s.slot_id <= prev {
                return Err(FormatError::not_canonical(
                    WHAT,
                    "slots not sorted and unique",
                ));
            }
            prev = s.slot_id;
        }
        if self.next_slot_id <= prev {
            return Err(FormatError::invalid(
                WHAT,
                "next_slot_id not above every slot_id",
            ));
        }
        Ok(())
    }

    /// Appends the keyring, `mac` included.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let start = out.len();
        self.encode_body(out)?;
        out.extend_from_slice(&self.mac);
        if out.len() - start > MAX_LEN {
            return Err(FormatError::range(
                WHAT,
                "length",
                (out.len() - start) as u64,
            ));
        }
        Ok(())
    }

    /// Appends every byte before the `mac`.
    pub fn encode_body(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        out.extend_from_slice(MAGIC);
        out.put_u16(VER);
        out.put_u8(self.root_kind as u8);
        out.put_u8(self.slots.len() as u8);
        out.put_u32(self.dek_gen.0);
        out.extend_from_slice(&self.family_id.0);
        out.put_u64(self.keyring_seq);
        out.put_u32(self.next_slot_id);
        out.put_zeros(4);
        for s in &self.slots {
            s.encode(out)?;
        }
        Ok(())
    }

    /// The bucket-relative name used in the MAC's AAD.
    pub fn name(&self) -> String {
        format!("families/{}/keyring", hex_lower(&self.family_id.0))
    }

    /// The bytes the keyring MAC covers:
    /// `meta_aad(family_id, 13, "families/<family-id>/keyring") ‖ keyring[0 .. end - 32]`.
    pub fn mac_input(&self) -> Result<Vec<u8>> {
        let mut v = meta_aad(&self.family_id.0, ObjectKind::Keyring, &self.name())?;
        self.encode_body(&mut v)?;
        Ok(v)
    }

    /// Decodes a whole keyring. Argon2id parameters must be inside the read
    /// bounds; the MAC is not checked here.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() > MAX_LEN {
            return Err(FormatError::range(WHAT, "length", buf.len() as u64));
        }
        if buf.len() < FIXED_LEN + MAC_LEN {
            return Err(FormatError::Truncated {
                what: WHAT,
                need: FIXED_LEN + MAC_LEN,
                have: buf.len(),
            });
        }
        let (body, mac_bytes) = buf.split_at(buf.len() - MAC_LEN);
        let mut mac = [0u8; 32];
        mac.copy_from_slice(mac_bytes);
        let mut r = Reader::new(body, WHAT);
        r.magic(MAGIC)?;
        r.version_u16(VER)?;
        let root_kind = match r.u8()? {
            1 => RootKind::Kmk,
            2 => RootKind::Ik,
            v => return Err(FormatError::range(WHAT, "root_kind", u64::from(v))),
        };
        let slot_count = r.u8()?;
        let dek_gen = DekGen(r.u32()?);
        let family_id = FamilyId(r.array()?);
        let keyring_seq = r.u64()?;
        let next_slot_id = r.u32()?;
        r.reserved(4)?;
        // A slot is at least 12 bytes.
        let n = r.check_count("slots", u64::from(slot_count), u64::from(MAX_SLOTS), 12)?;
        let mut slots = Vec::with_capacity(n);
        for _ in 0..n {
            slots.push(Slot::read(&mut r)?);
        }
        r.finish()?;
        let k = Self {
            root_kind,
            dek_gen,
            family_id,
            keyring_seq,
            next_slot_id,
            slots,
            mac,
        };
        k.validate()?;
        Ok(k)
    }
}

fn check_wrap_ver(v: u8) -> Result<()> {
    if v == WRAP_VER_XCHACHA {
        Ok(())
    } else {
        Err(FormatError::BadVersion {
            what: WHAT,
            got: u64::from(v),
        })
    }
}
