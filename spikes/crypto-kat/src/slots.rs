//! Key slots (ADR-11): the KMK wrapped under Argon2id-passphrase, raw key-file
//! or KMS slots. Wrapping is XChaCha20-Poly1305 with a random 24-byte nonce.
//! Unlike Constellation's `e2e.rs` (AAD `b"keyring-master"`), the AAD here binds
//! family id, slot id, slot kind and every KDF parameter (finding F8).

use aws_lc_rs::rand::fill as os_fill;
use chacha20poly1305::{
    aead::{Aead, Payload},
    KeyInit, XChaCha20Poly1305, XNonce,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Argon2Cfg {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Argon2Cfg {
    /// Constellation's defaults: 64 MiB, t=3, p=1.
    pub const DEFAULT: Argon2Cfg = Argon2Cfg {
        m_kib: 65_536,
        t: 3,
        p: 1,
    };
    /// Ceiling applied to parameters read from the bucket (availability defence:
    /// a hostile keyring must not make a client allocate gigabytes).
    pub const MAX_M_KIB: u32 = 1 << 20; // 1 GiB
    pub const MAX_T: u32 = 10;
    pub const MAX_P: u32 = 16;
    /// Floor enforced when *creating* a slot (not when opening one).
    pub const MIN_M_KIB: u32 = 19 * 1024; // OWASP minimum profile

    pub fn check_read(&self) -> Result<(), SlotError> {
        if self.m_kib > Self::MAX_M_KIB
            || self.t > Self::MAX_T
            || self.p > Self::MAX_P
            || self.m_kib < 8 * self.p
            || self.t == 0
            || self.p == 0
        {
            return Err(SlotError::BadParams);
        }
        Ok(())
    }
    pub fn check_create(&self) -> Result<(), SlotError> {
        self.check_read()?;
        if self.m_kib < Self::MIN_M_KIB {
            return Err(SlotError::BadParams);
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum SlotKind {
    Passphrase { cfg: Argon2Cfg, salt: [u8; 16] },
    KeyFile,
    Kms { key_ref: String },
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub id: u32,
    pub kind: SlotKind,
    /// version(1) || nonce(24) || ciphertext||tag   (for KMS: the KMS blob)
    pub blob: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Keyring {
    pub family: [u8; 16],
    pub slots: Vec<Slot>,
    pub next_id: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SlotError {
    BadParams,
    NoSuchSlot,
    WrongCredential,
    Corrupt,
    LastSlot,
}

pub enum NewSlot<'a> {
    Passphrase(&'a str, Argon2Cfg),
    KeyFile(&'a [u8; 32]),
    Kms(&'a FakeKms, &'a str),
}

pub enum Credential<'a> {
    Passphrase(&'a str),
    KeyFile(&'a [u8; 32]),
    Kms(&'a FakeKms),
}

/// Stand-in for a cloud KMS: AEAD under a key that never leaves it, with the
/// *encryption context* as AAD (AWS KMS EncryptionContext / GCP AAD).
pub struct FakeKms {
    key: [u8; 32],
}

impl FakeKms {
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        os_fill(&mut key).unwrap();
        Self { key }
    }
    fn canon(ctx: &[(&str, &str)]) -> Vec<u8> {
        let mut v: Vec<_> = ctx.to_vec();
        v.sort();
        let mut out = Vec::new();
        for (k, val) in v {
            out.extend_from_slice(&(k.len() as u32).to_le_bytes());
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(&(val.len() as u32).to_le_bytes());
            out.extend_from_slice(val.as_bytes());
        }
        out
    }
    pub fn encrypt(&self, ctx: &[(&str, &str)], pt: &[u8]) -> Vec<u8> {
        seal_x(&self.key, &Self::canon(ctx), pt)
    }
    pub fn decrypt(&self, ctx: &[(&str, &str)], blob: &[u8]) -> Result<Vec<u8>, SlotError> {
        open_x(&self.key, &Self::canon(ctx), blob).map_err(|_| SlotError::WrongCredential)
    }
}

impl Default for FakeKms {
    fn default() -> Self {
        Self::new()
    }
}

fn seal_x(key: &[u8; 32], aad: &[u8], pt: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; 24];
    os_fill(&mut nonce).unwrap();
    let c = XChaCha20Poly1305::new(key.into());
    let n = XNonce::try_from(&nonce[..]).unwrap();
    let ct = c.encrypt(&n, Payload { msg: pt, aad }).unwrap();
    let mut out = vec![1u8];
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

fn open_x(key: &[u8; 32], aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, ()> {
    if blob.len() < 1 + 24 + 16 || blob[0] != 1 {
        return Err(());
    }
    let c = XChaCha20Poly1305::new(key.into());
    let n = XNonce::try_from(&blob[1..25]).map_err(|_| ())?;
    c.decrypt(
        &n,
        Payload {
            msg: &blob[25..],
            aad,
        },
    )
    .map_err(|_| ())
}

fn kind_bytes(kind: &SlotKind) -> Vec<u8> {
    let mut v = Vec::new();
    match kind {
        SlotKind::Passphrase { cfg, salt } => {
            v.push(1);
            v.extend_from_slice(&cfg.m_kib.to_le_bytes());
            v.extend_from_slice(&cfg.t.to_le_bytes());
            v.extend_from_slice(&cfg.p.to_le_bytes());
            v.extend_from_slice(salt);
        }
        SlotKind::KeyFile => v.push(2),
        SlotKind::Kms { key_ref } => {
            v.push(3);
            v.extend_from_slice(&(key_ref.len() as u32).to_le_bytes());
            v.extend_from_slice(key_ref.as_bytes());
        }
    }
    v
}

pub fn slot_aad(family: &[u8; 16], id: u32, kind: &SlotKind) -> Vec<u8> {
    let mut a = b"lunix slot v1\0".to_vec();
    a.extend_from_slice(family);
    a.extend_from_slice(&id.to_le_bytes());
    a.extend_from_slice(&kind_bytes(kind));
    a
}

fn argon_kek(
    pass: &str,
    salt: &[u8; 16],
    cfg: Argon2Cfg,
) -> Result<Zeroizing<[u8; 32]>, SlotError> {
    use argon2::{Algorithm, Argon2, Params, Version};
    cfg.check_read()?;
    let params =
        Params::new(cfg.m_kib, cfg.t, cfg.p, Some(32)).map_err(|_| SlotError::BadParams)?;
    let a = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    a.hash_password_into(pass.as_bytes(), salt, &mut out[..])
        .map_err(|_| SlotError::BadParams)?;
    Ok(out)
}

fn kms_ctx(family: &[u8; 16], id: u32) -> (String, String) {
    (hex::encode(family), id.to_string())
}

impl Keyring {
    pub fn create(family: [u8; 16], kmk: &[u8; 32], first: NewSlot) -> Result<Self, SlotError> {
        let mut k = Keyring {
            family,
            slots: vec![],
            next_id: 1,
        };
        k.add_slot(kmk, first)?;
        Ok(k)
    }

    pub fn add_slot(&mut self, kmk: &[u8; 32], s: NewSlot) -> Result<u32, SlotError> {
        let id = self.next_id;
        let (kind, blob) = match s {
            NewSlot::Passphrase(p, cfg) => {
                cfg.check_create()?;
                let mut salt = [0u8; 16];
                os_fill(&mut salt).unwrap();
                let kind = SlotKind::Passphrase { cfg, salt };
                let kek = argon_kek(p, &salt, cfg)?;
                let blob = seal_x(&kek, &slot_aad(&self.family, id, &kind), kmk);
                (kind, blob)
            }
            NewSlot::KeyFile(kf) => {
                let kind = SlotKind::KeyFile;
                let blob = seal_x(kf, &slot_aad(&self.family, id, &kind), kmk);
                (kind, blob)
            }
            NewSlot::Kms(kms, key_ref) => {
                let kind = SlotKind::Kms {
                    key_ref: key_ref.to_string(),
                };
                let (f, i) = kms_ctx(&self.family, id);
                let blob = kms.encrypt(
                    &[
                        ("lunix:family", &f),
                        ("lunix:slot", &i),
                        ("lunix:ref", key_ref),
                    ],
                    kmk,
                );
                (kind, blob)
            }
        };
        self.slots.push(Slot { id, kind, blob });
        self.next_id += 1;
        Ok(id)
    }

    /// Removing a slot deletes one wrapped copy of the KMK from *this* keyring
    /// object. It revokes nothing already exposed (see tests).
    pub fn remove_slot(&mut self, id: u32) -> Result<(), SlotError> {
        if self.slots.len() == 1 {
            return Err(SlotError::LastSlot);
        }
        let n = self.slots.len();
        self.slots.retain(|s| s.id != id);
        if self.slots.len() == n {
            return Err(SlotError::NoSuchSlot);
        }
        Ok(())
    }

    pub fn unwrap(&self, id: u32, cred: Credential) -> Result<Zeroizing<[u8; 32]>, SlotError> {
        let s = self
            .slots
            .iter()
            .find(|s| s.id == id)
            .ok_or(SlotError::NoSuchSlot)?;
        let aad = slot_aad(&self.family, s.id, &s.kind);
        let raw = match (&s.kind, cred) {
            (SlotKind::Passphrase { cfg, salt }, Credential::Passphrase(p)) => {
                let kek = argon_kek(p, salt, *cfg)?;
                open_x(&kek, &aad, &s.blob).map_err(|_| SlotError::WrongCredential)?
            }
            (SlotKind::KeyFile, Credential::KeyFile(kf)) => {
                open_x(kf, &aad, &s.blob).map_err(|_| SlotError::WrongCredential)?
            }
            (SlotKind::Kms { key_ref }, Credential::Kms(kms)) => {
                let (f, i) = kms_ctx(&self.family, s.id);
                kms.decrypt(
                    &[
                        ("lunix:family", &f),
                        ("lunix:slot", &i),
                        ("lunix:ref", key_ref),
                    ],
                    &s.blob,
                )?
            }
            _ => return Err(SlotError::WrongCredential),
        };
        let arr: [u8; 32] = raw.try_into().map_err(|_| SlotError::Corrupt)?;
        Ok(Zeroizing::new(arr))
    }

    /// Passphrase change = re-wrap of the same KMK (nothing else changes).
    pub fn rewrap_passphrase(
        &mut self,
        id: u32,
        old: &str,
        new: &str,
        cfg: Argon2Cfg,
    ) -> Result<(), SlotError> {
        let kmk = self.unwrap(id, Credential::Passphrase(old))?;
        cfg.check_create()?;
        let mut salt = [0u8; 16];
        os_fill(&mut salt).unwrap();
        let kind = SlotKind::Passphrase { cfg, salt };
        let kek = argon_kek(new, &salt, cfg)?;
        let blob = seal_x(&kek, &slot_aad(&self.family, id, &kind), &kmk[..]);
        let s = self
            .slots
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(SlotError::NoSuchSlot)?;
        s.kind = kind;
        s.blob = blob;
        Ok(())
    }
}
