//! Metadata sealing (commits, DELTA, lease, descriptor): XChaCha20-Poly1305,
//! random 24-byte nonce, one key per object kind, AAD = object identity.
//! Plus the chain / lease / attach checks the review proposes.

use crate::kdf::{purpose_key, Purpose};
use aws_lc_rs::rand::fill as os_fill;
use chacha20poly1305::{
    aead::{Aead, Payload},
    KeyInit, XChaCha20Poly1305, XNonce,
};

#[derive(Debug, PartialEq, Eq)]
pub struct MetaError;

pub struct ObjId<'a> {
    pub volume: &'a [u8; 16],
    pub kind: u8,
    /// object key relative to the volume prefix, e.g. `commits/0000000000000007`
    pub name: &'a str,
}

fn aad(id: &ObjId) -> Vec<u8> {
    let mut a = b"lunix meta v1\0".to_vec();
    a.extend_from_slice(id.volume);
    a.push(id.kind);
    a.extend_from_slice(&(id.name.len() as u32).to_le_bytes());
    a.extend_from_slice(id.name.as_bytes());
    a
}

pub fn seal_meta(key: &[u8; 32], id: &ObjId, pt: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; 24];
    os_fill(&mut nonce).unwrap();
    let c = XChaCha20Poly1305::new(key.into());
    let ct = c
        .encrypt(
            &XNonce::try_from(&nonce[..]).unwrap(),
            Payload {
                msg: pt,
                aad: &aad(id),
            },
        )
        .unwrap();
    let mut out = vec![1u8];
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

pub fn open_meta(key: &[u8; 32], id: &ObjId, blob: &[u8]) -> Result<Vec<u8>, MetaError> {
    if blob.len() < 1 + 24 + 16 || blob[0] != 1 {
        return Err(MetaError);
    }
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            &XNonce::try_from(&blob[1..25]).map_err(|_| MetaError)?,
            Payload {
                msg: &blob[25..],
                aad: &aad(id),
            },
        )
        .map_err(|_| MetaError)
}

pub fn meta_key(kmk: &[u8; 32], family: &[u8; 16], kind: u8) -> [u8; 32] {
    *purpose_key(kmk, family, Purpose::Meta { kind })
}

// ---- commits --------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub seq: u64,
    pub epoch: u64,
    /// Proposed (finding F5): hash of the parent commit *object*. DESIGN §8.2 has only `parent_seq`.
    pub parent_hash: [u8; 32],
    pub body: Vec<u8>,
}

impl Commit {
    fn encode(&self) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&self.seq.to_le_bytes());
        v.extend_from_slice(&self.epoch.to_le_bytes());
        v.extend_from_slice(&self.parent_hash);
        v.extend_from_slice(&self.body);
        v
    }
    fn decode(b: &[u8]) -> Option<Commit> {
        if b.len() < 48 {
            return None;
        }
        Some(Commit {
            seq: u64::from_le_bytes(b[0..8].try_into().ok()?),
            epoch: u64::from_le_bytes(b[8..16].try_into().ok()?),
            parent_hash: b[16..48].try_into().ok()?,
            body: b[48..].to_vec(),
        })
    }
}

pub fn commit_name(seq: u64) -> String {
    format!("commits/{seq:016x}")
}

pub fn seal_commit(key: &[u8; 32], vol: &[u8; 16], c: &Commit) -> Vec<u8> {
    seal_meta(
        key,
        &ObjId {
            volume: vol,
            kind: crate::kdf::META_COMMIT,
            name: &commit_name(c.seq),
        },
        &c.encode(),
    )
}

pub fn object_hash(sealed: &[u8]) -> [u8; 32] {
    *blake3::hash(sealed).as_bytes()
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChainError {
    Open(u64),
    SeqMismatch(u64),
    ParentHash(u64),
    EpochRegress(u64),
}

/// Open every commit and check the chain. With `require_parent_hash == false`
/// this is today's DESIGN (only `seq` links commits).
pub fn verify_chain(
    key: &[u8; 32],
    vol: &[u8; 16],
    objs: &[(u64, Vec<u8>)],
    require_parent_hash: bool,
) -> Result<(), ChainError> {
    let mut prev: Option<(Vec<u8>, u64)> = None;
    for (seq, sealed) in objs {
        let id = ObjId {
            volume: vol,
            kind: crate::kdf::META_COMMIT,
            name: &commit_name(*seq),
        };
        let pt = open_meta(key, &id, sealed).map_err(|_| ChainError::Open(*seq))?;
        let c = Commit::decode(&pt).ok_or(ChainError::Open(*seq))?;
        if c.seq != *seq {
            return Err(ChainError::SeqMismatch(*seq));
        }
        if let Some((psealed, pepoch)) = &prev {
            if require_parent_hash && c.parent_hash != object_hash(psealed) {
                return Err(ChainError::ParentHash(*seq));
            }
            if c.epoch < *pepoch {
                return Err(ChainError::EpochRegress(*seq));
            }
        }
        prev = Some((sealed.clone(), c.epoch));
    }
    Ok(())
}

// ---- lease ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub epoch: u64,
    pub released: bool,
}

pub fn seal_lease(key: &[u8; 32], vol: &[u8; 16], l: &Lease) -> Vec<u8> {
    let mut pt = l.epoch.to_le_bytes().to_vec();
    pt.push(l.released as u8);
    // AAD is identical for every version of the mutable `lease` object.
    seal_meta(
        key,
        &ObjId {
            volume: vol,
            kind: crate::kdf::META_LEASE,
            name: "lease",
        },
        &pt,
    )
}

pub fn open_lease(key: &[u8; 32], vol: &[u8; 16], blob: &[u8]) -> Result<Lease, MetaError> {
    let pt = open_meta(
        key,
        &ObjId {
            volume: vol,
            kind: crate::kdf::META_LEASE,
            name: "lease",
        },
        blob,
    )?;
    if pt.len() != 9 {
        return Err(MetaError);
    }
    Ok(Lease {
        epoch: u64::from_le_bytes(pt[..8].try_into().unwrap()),
        released: pt[8] != 0,
    })
}

/// Proposed rule: a lease whose epoch is below the newest epoch seen in the
/// verified commit chain (or in local state) is a replay, whatever its AEAD says.
pub fn lease_is_stale(lease: &Lease, max_epoch_seen: u64) -> bool {
    lease.epoch < max_epoch_seen
}

// ---- volume descriptor / attach policy -------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub encrypted: bool,
    pub family: [u8; 16],
    pub chunk_size: u32,
    pub id_len: u8,
    pub kmk_fp: [u8; 16],
}

impl Descriptor {
    fn canon(&self) -> Vec<u8> {
        let mut v = b"lunix volume v1\0".to_vec();
        v.push(self.encrypted as u8);
        v.extend_from_slice(&self.family);
        v.extend_from_slice(&self.chunk_size.to_le_bytes());
        v.push(self.id_len);
        v.extend_from_slice(&self.kmk_fp);
        v
    }
}

pub fn kmk_fingerprint(kmk: &[u8; 32], family: &[u8; 16]) -> [u8; 16] {
    let k = purpose_key(kmk, family, Purpose::Pin);
    k[..16].try_into().unwrap()
}

pub fn descriptor_mac(kmk: &[u8; 32], d: &Descriptor) -> [u8; 32] {
    let k = purpose_key(kmk, &d.family, Purpose::VolumeMac);
    *blake3::keyed_hash(&k, &d.canon()).as_bytes()
}

pub enum Expect {
    /// The caller supplied key material (passphrase, key file, KMS ref): the
    /// volume *must* be encrypted, and with this KMK if a fingerprint was supplied.
    Encrypted { kmk_fp: Option<[u8; 16]> },
    /// The caller supplied no key material (plain volume expected).
    Plain,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AttachError {
    Downgrade,
    BadMac,
    WrongKmk,
    UnexpectedEncrypted,
}

/// `mac_ok` is the result of verifying `descriptor_mac` after unwrapping the KMK
/// (None when no key material was supplied and so no MAC can be checked).
pub fn check_attach(
    expect: &Expect,
    d: &Descriptor,
    mac_ok: Option<bool>,
) -> Result<(), AttachError> {
    match expect {
        Expect::Encrypted { kmk_fp } => {
            if !d.encrypted {
                return Err(AttachError::Downgrade);
            }
            if mac_ok != Some(true) {
                return Err(AttachError::BadMac);
            }
            if let Some(fp) = kmk_fp {
                if *fp != d.kmk_fp {
                    return Err(AttachError::WrongKmk);
                }
            }
            Ok(())
        }
        Expect::Plain => {
            if d.encrypted {
                Err(AttachError::UnexpectedEncrypted)
            } else {
                Ok(())
            }
        }
    }
}
