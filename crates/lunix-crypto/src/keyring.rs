//! The family keyring (ADR-11 F6, on-disk-formats.md section 12).
//!
//! One mutable object per family. This module creates it, adds, removes and
//! rewraps slots, rotates the DEK generation and unlocks it; storing it is the
//! caller's job, as a compare-and-swap (`If-Match`) write on the authority that
//! is copied byte for byte to the mirrors. Every change returns a new
//! [`Keyring`] with `keyring_seq` raised by one and a fresh MAC; a keyring
//! below the highest `keyring_seq` seen is refused by [`check_not_older`].
//!
//! # Slots
//!
//! A slot wraps the family root (the KMK or the integrity key) with
//! XChaCha20-Poly1305 under a key-encryption key, with the AAD built by
//! `lunix_format::keyring::Slot::aad` (family, slot id, kind and every KDF
//! parameter). Implemented kinds: passphrase (Argon2id, parameters stored per
//! slot) and key file (32 random bytes). KMS slots decode and then answer
//! `Unsupported`.
//!
//! # Bounds
//!
//! Argon2id parameters are bounded when read (at most 1 GiB, `t` at most 10,
//! `p` at most 16, enforced by the decoder before anything is allocated) and
//! floored when written (at least 19 MiB and `t` of 2).
//!
//! # Errors
//!
//! A wrong passphrase, a wrong key file, a corrupted wrap, a keyring for
//! another family and a bad keyring MAC are one error,
//! [`KeyringError::WrongCredential`], after the same work.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use lunix_format::keyring::{
    Argon2Params, Keyring, RootKind, Slot, SlotParams, SlotWrap, ARGON_WRITE_MIN_M_KIB,
    ARGON_WRITE_MIN_T, MAX_SLOTS, WRAP_VER_XCHACHA,
};
use lunix_format::purpose::Purpose;
use lunix_format::{DekGen, FamilyId, FormatError, Profile};
use lunix_iface::Error;
use zeroize::Zeroizing;

use crate::secret::{Root, Secret};
use crate::{ct, kdf, mac, rng};

/// Why a keyring operation failed. Variants name a class, never a value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeyringError {
    /// The credential does not unlock the keyring (wrong, or the keyring is
    /// damaged: the two are not told apart).
    #[error("the credential does not unlock the keyring")]
    WrongCredential,
    /// The keyring's `keyring_seq` is below one already seen.
    #[error("the keyring is older than one already seen")]
    Rollback,
    /// The bytes are not a valid keyring (including Argon2id parameters over
    /// the read bounds).
    #[error("invalid keyring: {0}")]
    Format(#[from] FormatError),
    /// Argon2id parameters below the floor for a new slot.
    #[error("argon2id parameters are below the floor for new slots")]
    BelowFloor,
    /// A slot kind or feature that phase 1 does not implement.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// A precondition of the call does not hold.
    #[error("invalid argument: {0}")]
    Invalid(&'static str),
    /// Every matching slot is above the caller's cost policy (see
    /// [`UnlockPolicy`]); nothing was derived for those slots.
    #[error("every matching slot is above the unlock cost policy")]
    OverPolicy,
    /// A key file could not be read or written.
    #[error("key file i/o: {0}")]
    Io(#[from] std::io::Error),
}

impl From<Error> for KeyringError {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => Self::Io(e),
            Error::Format(f) => Self::Format(f),
            _ => Self::Invalid("internal error"),
        }
    }
}

impl From<KeyringError> for Error {
    fn from(e: KeyringError) -> Self {
        match e {
            KeyringError::WrongCredential => Error::Auth { what: "keyring" },
            KeyringError::Rollback => Error::Refused {
                why: "keyring is older than one already seen",
            },
            KeyringError::Format(f) => Error::Format(f),
            KeyringError::BelowFloor => Error::Invalid {
                why: "argon2id parameters below the floor",
            },
            KeyringError::OverPolicy => Error::Refused {
                why: "argon2id cost above the unlock policy",
            },
            KeyringError::Unsupported(what) => Error::Unsupported { what },
            KeyringError::Invalid(why) => Error::Invalid { why },
            KeyringError::Io(e) => Error::Io(e),
        }
    }
}

type Result<T, E = KeyringError> = std::result::Result<T, E>;

/// Argon2id cost of a new passphrase slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Argon2Cost {
    /// Memory in KiB (at least 19 456 to write).
    pub m_kib: u32,
    /// Iterations (at least 2 to write).
    pub t: u32,
    /// Lanes.
    pub p: u32,
}

impl Argon2Cost {
    /// 64 MiB, `t` = 3, `p` = 1: Constellation's defaults, 75-83 ms per wrap
    /// on the phase 0 host.
    pub const DEFAULT: Self = Self {
        m_kib: 65_536,
        t: 3,
        p: 1,
    };
}

impl Default for Argon2Cost {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A raw 32-byte key file: the key-encryption key of a kind-2 slot.
pub struct KeyFile(Secret);

impl KeyFile {
    /// A fresh random key file.
    pub fn generate() -> Result<Self> {
        let mut b = Zeroizing::new([0u8; 32]);
        rng::fill_os(&mut b[..])?;
        Ok(Self(Secret::new(&b)))
    }

    /// A key file from exactly 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let b: &[u8; 32] = bytes
            .try_into()
            .map_err(|_| KeyringError::Invalid("a key file is exactly 32 bytes"))?;
        Ok(Self(Secret::new(b)))
    }

    /// Reads a key file. Any other length is refused. The path is opened with
    /// `O_NOFOLLOW | O_NOCTTY | O_NONBLOCK` and must be a regular file, so a
    /// symlink is not followed and a FIFO or device cannot block the attach.
    pub fn read(path: &Path) -> Result<Self> {
        Self::read_noting_mode(path).map(|(kf, _)| kf)
    }

    /// Like [`Self::read`], and also says whether group or other permission
    /// bits are set. A caller logs that, but must not refuse: a Kubernetes
    /// Secret mount is not 0600.
    pub fn read_noting_mode(path: &Path) -> Result<(Self, bool)> {
        use std::os::unix::fs::PermissionsExt;
        let flags = rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NOCTTY
            | rustix::fs::OFlags::NONBLOCK;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                i32::try_from(flags.bits()).map_err(|_| KeyringError::Invalid("open flags"))?,
            )
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(KeyringError::Invalid("a key file must be a regular file"));
        }
        let loose = meta.permissions().mode() & 0o077 != 0;
        let mut buf = Zeroizing::new(Vec::with_capacity(33));
        file.take(33).read_to_end(&mut buf)?;
        Ok((Self::from_bytes(&buf)?, loose))
    }

    /// Creates the file with mode 0600, failing if it exists, and makes it
    /// durable (data and directory).
    pub fn write_new(&self, path: &Path) -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(self.0.expose())?;
        f.sync_all()?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }

    /// The bytes, for known-answer tests only.
    #[cfg(feature = "test-rng")]
    pub fn expose_for_tests(&self) -> [u8; 32] {
        *self.0.expose()
    }
}

/// The secret material of a new slot.
pub enum NewSlot<'a> {
    /// A passphrase through Argon2id with a fresh random salt.
    Passphrase {
        /// The passphrase (not empty).
        passphrase: &'a [u8],
        /// The cost; floored when written.
        cost: Argon2Cost,
    },
    /// A raw key file.
    KeyFile(&'a KeyFile),
    /// A KMS key reference: accepted by the decoder, `Unsupported` here.
    Kms {
        /// The key reference.
        key_ref: &'a str,
    },
}

/// What the caller offers to unwrap a slot.
pub enum Unlock<'a> {
    /// A passphrase: tries the passphrase slots.
    Passphrase(&'a [u8]),
    /// A key file already in memory: tries the key-file slots.
    KeyFile(&'a KeyFile),
    /// A key file to read now. A missing or short file is a wrong credential.
    KeyFilePath(&'a Path),
    /// A KMS reference: `Unsupported` in phase 1.
    Kms(&'a str),
}

/// How much work `unlock` may spend on a keyring it does not trust: the read
/// bounds of the format (1 GiB, `t` 10, `p` 16, 32 slots) are far above any
/// real slot, and a bucket writer could otherwise cost an attach minutes of CPU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UnlockPolicy {
    /// A passphrase slot with more memory than this (KiB) is skipped.
    pub max_m_kib: u32,
    /// Total of `m_kib * t` over the passphrase slots tried in one call.
    pub max_total_work: u64,
}

impl Default for UnlockPolicy {
    /// 256 MiB per slot, and at most four slots at 256 MiB and `t` = 3.
    fn default() -> Self {
        Self {
            max_m_kib: 262_144,
            max_total_work: 4 * 262_144 * 3,
        }
    }
}

/// Options of [`unlock`].
#[derive(Clone, Copy, Default, Debug)]
pub struct UnlockOptions {
    /// Try only this slot.
    pub only_slot: Option<u32>,
    /// Try this slot first (the one that unwrapped at the last attach), so a
    /// normal attach does one derivation however many slots the keyring holds.
    pub preferred_slot: Option<u32>,
    /// The cost ceiling.
    pub policy: UnlockPolicy,
}

/// A keyring that was just created.
pub struct Created {
    /// The new family root.
    pub root: Root,
    /// The keyring, MAC included.
    pub keyring: Keyring,
    /// Its encoding, ready to store.
    pub bytes: Vec<u8>,
    /// The id of the first slot.
    pub slot_id: u32,
}

/// A keyring that was unwrapped.
pub struct Unlocked {
    /// The family root the slot wraps.
    pub root: Root,
    /// The decoded keyring (its MAC verified under `root`).
    pub keyring: Keyring,
    /// The slot that unwrapped.
    pub slot_id: u32,
}

/// The KEK of a passphrase slot, for the Argon2id known-answer vector.
#[cfg(feature = "test-rng")]
pub fn passphrase_kek(passphrase: &[u8], params: &Argon2Params) -> Result<Zeroizing<[u8; 32]>> {
    argon2id_kek(passphrase, params)
}

fn kms_unsupported() -> KeyringError {
    KeyringError::Unsupported("KMS key slots (phase 3)")
}

fn argon2id_kek(passphrase: &[u8], p: &Argon2Params) -> Result<Zeroizing<[u8; 32]>> {
    p.check_read_bounds()?;
    let params = Params::new(p.m_kib, p.t, p.p, Some(32))
        .map_err(|_| KeyringError::Invalid("argon2id parameters"))?;
    let mut out = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase, &p.salt, &mut out[..])
        .map_err(|_| KeyringError::Invalid("argon2id derivation"))?;
    Ok(out)
}

fn xnonce(nonce: &[u8; 24]) -> Result<XNonce> {
    XNonce::try_from(&nonce[..]).map_err(|_| KeyringError::Invalid("nonce length"))
}

/// Wraps `root` for a new or rewrapped slot.
fn make_slot(family: &FamilyId, slot_id: u32, new: &NewSlot<'_>, root: &[u8; 32]) -> Result<Slot> {
    let (params, kek): (SlotParams, Zeroizing<[u8; 32]>) = match new {
        NewSlot::Passphrase { passphrase, cost } => {
            if passphrase.is_empty() {
                return Err(KeyringError::Invalid("empty passphrase"));
            }
            let mut salt = [0u8; 16];
            rng::fill_os(&mut salt)?;
            let p = Argon2Params {
                m_kib: cost.m_kib,
                t: cost.t,
                p: cost.p,
                salt,
            };
            p.check_read_bounds()?;
            if p.m_kib < ARGON_WRITE_MIN_M_KIB || p.t < ARGON_WRITE_MIN_T {
                return Err(KeyringError::BelowFloor);
            }
            let kek = argon2id_kek(passphrase, &p)?;
            (SlotParams::Passphrase(p), kek)
        }
        NewSlot::KeyFile(kf) => (SlotParams::KeyFile, Zeroizing::new(*kf.0.expose())),
        NewSlot::Kms { .. } => return Err(kms_unsupported()),
    };
    let mut slot = Slot {
        slot_id,
        params,
        wrap: SlotWrap::Key {
            wrap_ver: WRAP_VER_XCHACHA,
            nonce: [0; 24],
            ciphertext: [0; 32],
            tag: [0; 16],
        },
    };
    // The AAD covers everything but the wrap itself.
    let aad = slot.aad(family);
    let mut nonce = [0u8; 24];
    rng::fill_os(&mut nonce)?;
    let sealed = XChaCha20Poly1305::new((&*kek).into())
        .encrypt(
            &xnonce(&nonce)?,
            Payload {
                msg: root,
                aad: &aad,
            },
        )
        .map_err(|_| KeyringError::Invalid("slot wrap"))?;
    let (c, t) = sealed.split_at(32);
    slot.wrap = SlotWrap::Key {
        wrap_ver: WRAP_VER_XCHACHA,
        nonce,
        ciphertext: c
            .try_into()
            .map_err(|_| KeyringError::Invalid("slot wrap length"))?,
        tag: t
            .try_into()
            .map_err(|_| KeyringError::Invalid("slot wrap length"))?,
    };
    Ok(slot)
}

/// Opens one slot with `kek`; `None` for any failure.
fn open_slot(family: &FamilyId, slot: &Slot, kek: &[u8; 32]) -> Option<Zeroizing<[u8; 32]>> {
    let SlotWrap::Key {
        wrap_ver,
        nonce,
        ciphertext,
        tag,
    } = &slot.wrap
    else {
        return None;
    };
    if *wrap_ver != WRAP_VER_XCHACHA {
        return None;
    }
    let aad = slot.aad(family);
    let mut joined = Zeroizing::new(Vec::with_capacity(48));
    joined.extend_from_slice(ciphertext);
    joined.extend_from_slice(tag);
    let plain = XChaCha20Poly1305::new(kek.into())
        .decrypt(
            &xnonce(nonce).ok()?,
            Payload {
                msg: &joined,
                aad: &aad,
            },
        )
        .ok()?;
    let plain = Zeroizing::new(plain);
    let root: [u8; 32] = plain.as_slice().try_into().ok()?;
    Some(Zeroizing::new(root))
}

fn mac_key(profile: Profile, root: &Root, family: &FamilyId) -> Result<Zeroizing<[u8; 32]>> {
    Ok(kdf::purpose_key(
        profile,
        root,
        family,
        &Purpose::KeyringMac,
    )?)
}

fn compute_mac(kr: &Keyring, profile: Profile, root: &Root) -> Result<[u8; 32]> {
    let key = mac_key(profile, root, &kr.family_id)?;
    let input = kr.mac_input()?;
    Ok(mac::hmac_sha256(&key, &[&input]))
}

/// True if the keyring's MAC verifies under `root`.
pub fn verify_mac(kr: &Keyring, profile: Profile, root: &Root) -> bool {
    root.kind() == kr.root_kind && compute_mac(kr, profile, root).is_ok_and(|m| ct::eq(&m, &kr.mac))
}

/// Refuses a keyring below the highest `keyring_seq` seen.
///
/// # Errors
/// [`KeyringError::Rollback`].
pub fn check_not_older(kr: &Keyring, seen: u64) -> Result<()> {
    if kr.keyring_seq < seen {
        Err(KeyringError::Rollback)
    } else {
        Ok(())
    }
}

/// The keyring's encoding.
pub fn encode(kr: &Keyring) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    kr.encode(&mut out)?;
    Ok(out)
}

/// Creates a keyring for a new family around a fresh root of `kind`, with
/// `first` as slot 1.
///
/// # Errors
/// `BelowFloor` for Argon2id parameters under the floor; `Unsupported` for a
/// KMS slot.
pub fn create(
    profile: Profile,
    family: FamilyId,
    kind: RootKind,
    first: &NewSlot<'_>,
) -> Result<Created> {
    create_with_root(profile, family, Root::generate(kind)?, first)
}

/// Like [`create`] around a root the caller already holds.
pub fn create_with_root(
    profile: Profile,
    family: FamilyId,
    root: Root,
    first: &NewSlot<'_>,
) -> Result<Created> {
    let slot = make_slot(&family, 1, first, root.expose())?;
    let mut keyring = Keyring {
        root_kind: root.kind(),
        dek_gen: DekGen(0),
        family_id: family,
        keyring_seq: 1,
        next_slot_id: 2,
        slots: vec![slot],
        mac: [0; 32],
    };
    keyring.mac = compute_mac(&keyring, profile, &root)?;
    let bytes = encode(&keyring)?;
    Ok(Created {
        root,
        keyring,
        bytes,
        slot_id: 1,
    })
}

/// Creates the keyring of a default (`integrity`) volume made without a
/// passphrase: a fresh integrity key wrapped by a key-file slot whose random
/// key is written to `key_file` with mode 0600 (D16, `<dir>/<uuid>/integrity.key`).
/// If the keyring cannot be built the key file is removed again.
pub fn create_integrity_with_key_file(family: FamilyId, key_file: &Path) -> Result<Created> {
    let kf = KeyFile::generate()?;
    kf.write_new(key_file)?;
    match create(
        Profile::Default,
        family,
        RootKind::Ik,
        &NewSlot::KeyFile(&kf),
    ) {
        Ok(c) => Ok(c),
        Err(e) => {
            // Best effort: the file holds a key no keyring wraps.
            let _ = std::fs::remove_file(key_file);
            Err(e)
        }
    }
}

/// Unwraps a slot with `cred` and verifies the keyring.
///
/// Decodes (bounded), tries the slots of the credential's kind (the
/// preferred slot first, only `only_slot` when given, passphrase slots within
/// the cost policy), checks the MAC under the unwrapped root, then checks
/// `keyring_seq` against `seen_seq`.
///
/// # Errors
/// [`KeyringError::WrongCredential`] for every failure to unwrap or verify;
/// `Format` for malformed bytes or Argon2id parameters over the read bounds;
/// `OverPolicy` when only slots above the policy matched; `Rollback`;
/// `Unsupported` for a KMS credential.
pub fn unlock(
    bytes: &[u8],
    profile: Profile,
    family: &FamilyId,
    cred: &Unlock<'_>,
    seen_seq: u64,
    opts: &UnlockOptions,
) -> Result<Unlocked> {
    let kr = Keyring::decode(bytes)?;
    let key_file;
    let (want_kind, key_file_ref): (u8, Option<&KeyFile>) = match cred {
        Unlock::Passphrase(_) => (1, None),
        Unlock::KeyFile(kf) => (2, Some(kf)),
        Unlock::KeyFilePath(p) => match KeyFile::read(p) {
            Ok(kf) => {
                key_file = kf;
                (2, Some(&key_file))
            }
            // A missing or short file unlocks nothing, like a wrong one.
            Err(_) => return Err(KeyringError::WrongCredential),
        },
        Unlock::Kms(_) => return Err(kms_unsupported()),
    };
    if kr.family_id != *family {
        return Err(KeyringError::WrongCredential);
    }
    let mut order: Vec<&Slot> = kr.slots.iter().collect();
    if let Some(first) = opts.preferred_slot {
        // Stable: the preferred slot moves to the front, the rest keep order.
        order.sort_by_key(|s| s.slot_id != first);
    }
    let mut work = 0u64;
    let mut skipped = false;
    for slot in order {
        if slot.params.kind() != want_kind || opts.only_slot.is_some_and(|id| id != slot.slot_id) {
            continue;
        }
        let kek: Zeroizing<[u8; 32]> = match (&slot.params, cred, key_file_ref) {
            (SlotParams::Passphrase(p), Unlock::Passphrase(pw), _) => {
                let cost = u64::from(p.m_kib) * u64::from(p.t);
                if p.m_kib > opts.policy.max_m_kib
                    || work.saturating_add(cost) > opts.policy.max_total_work
                {
                    skipped = true;
                    continue;
                }
                work += cost;
                argon2id_kek(pw, p)?
            }
            (SlotParams::KeyFile, _, Some(kf)) => Zeroizing::new(*kf.0.expose()),
            _ => continue,
        };
        let Some(root_bytes) = open_slot(family, slot, &kek) else {
            continue;
        };
        let root = Root::from_bytes(kr.root_kind, &root_bytes);
        if !verify_mac(&kr, profile, &root) {
            continue;
        }
        check_not_older(&kr, seen_seq)?;
        return Ok(Unlocked {
            root,
            keyring: kr.clone(),
            slot_id: slot.slot_id,
        });
    }
    if skipped {
        return Err(KeyringError::OverPolicy);
    }
    Err(KeyringError::WrongCredential)
}

/// Raises `keyring_seq` and re-MACs. The caller already proved it holds `root`.
fn touch(kr: &mut Keyring, profile: Profile, root: &Root) -> Result<()> {
    kr.keyring_seq = kr
        .keyring_seq
        .checked_add(1)
        .ok_or(KeyringError::Invalid("keyring_seq overflow"))?;
    kr.mac = compute_mac(kr, profile, root)?;
    Ok(())
}

/// The caller must hold the root of this very keyring.
fn require_root(kr: &Keyring, profile: Profile, root: &Root) -> Result<()> {
    if verify_mac(kr, profile, root) {
        Ok(())
    } else {
        Err(KeyringError::WrongCredential)
    }
}

/// Adds a slot wrapping `root`; returns the new keyring and the slot's id.
/// Slot ids are never reused.
pub fn add_slot(
    kr: &Keyring,
    profile: Profile,
    root: &Root,
    new: &NewSlot<'_>,
) -> Result<(Keyring, u32)> {
    require_root(kr, profile, root)?;
    if kr.slots.len() >= usize::from(MAX_SLOTS) {
        return Err(KeyringError::Invalid("a keyring holds at most 32 slots"));
    }
    let id = kr.next_slot_id;
    let mut out = kr.clone();
    out.slots
        .push(make_slot(&kr.family_id, id, new, root.expose())?);
    out.next_slot_id = id
        .checked_add(1)
        .ok_or(KeyringError::Invalid("slot id overflow"))?;
    touch(&mut out, profile, root)?;
    Ok((out, id))
}

/// Removes a slot. The last slot cannot be removed. This revokes nothing the
/// holder of the slot's credential has already seen: the root is unchanged
/// (ADR-11), and revocation is migration to a new root.
pub fn remove_slot(kr: &Keyring, profile: Profile, root: &Root, slot_id: u32) -> Result<Keyring> {
    require_root(kr, profile, root)?;
    if kr.slots.len() == 1 {
        return Err(KeyringError::Invalid("the last slot cannot be removed"));
    }
    let mut out = kr.clone();
    let before = out.slots.len();
    out.slots.retain(|s| s.slot_id != slot_id);
    if out.slots.len() == before {
        return Err(KeyringError::Invalid("no such slot"));
    }
    touch(&mut out, profile, root)?;
    Ok(out)
}

/// Replaces the wrap of slot `slot_id` (a new passphrase or key file, a fresh
/// salt and nonce) without changing the root.
pub fn rewrap(
    kr: &Keyring,
    profile: Profile,
    root: &Root,
    slot_id: u32,
    new: &NewSlot<'_>,
) -> Result<Keyring> {
    require_root(kr, profile, root)?;
    let mut out = kr.clone();
    let pos = out
        .slots
        .iter()
        .position(|s| s.slot_id == slot_id)
        .ok_or(KeyringError::Invalid("no such slot"))?;
    out.slots[pos] = make_slot(&kr.family_id, slot_id, new, root.expose())?;
    touch(&mut out, profile, root)?;
    Ok(out)
}

/// Raises the DEK generation: new extents use the next one, old extents keep
/// theirs. Only a KMK keyring has generations.
pub fn rotate_dek(kr: &Keyring, profile: Profile, root: &Root) -> Result<Keyring> {
    require_root(kr, profile, root)?;
    if kr.root_kind != RootKind::Kmk {
        return Err(KeyringError::Invalid("an integrity keyring has no DEK"));
    }
    let mut out = kr.clone();
    out.dek_gen = DekGen(
        kr.dek_gen
            .0
            .checked_add(1)
            .ok_or(KeyringError::Invalid("dek_gen overflow"))?,
    );
    touch(&mut out, profile, root)?;
    Ok(out)
}
