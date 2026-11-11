//! The extent writer/reader model (DESIGN §5 step 6, §6 extent layout, ADR-11).

use crate::codec::{decompress_bounded, Codec, DecompressError};
use crate::kdf::{extent_key, EncDesc, KeyCtx, Profile, KIND_DATA};
use crate::tripwire::{Ledger, Tripwire};
use aws_lc_rs::aead::{Aad, LessSafeKey, Nonce, RandomizedNonceKey, UnboundKey, AES_256_GCM};
use aws_lc_rs::rand::fill as os_fill;
use zeroize::Zeroizing;

pub const FORMAT_VER: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub ver: u8,
    pub codec: u8,
    pub level: i8,
    pub ulen: u32,
    /// Not in the DESIGN §6 header today (finding F2). Modelled here because a
    /// reader cannot derive the key without it.
    pub dek_epoch: u32,
    pub chunk_id: [u8; 32],
    pub salt: [u8; 24],
    /// Only the FIPS profile stores a module-generated nonce.
    pub nonce: Option<[u8; 12]>,
}

#[derive(Clone, Debug)]
pub struct Extent {
    pub h: Header,
    /// ciphertext followed by the 16-byte GCM tag
    pub body: Vec<u8>,
}

pub struct SaltReq<'a> {
    pub chunk_id: &'a [u8],
    pub codec: u8,
    pub level: i8,
    pub payload: &'a [u8],
}

pub trait SaltSource {
    fn next_salt(&mut self, req: &SaltReq) -> [u8; 24];
}

/// OS-backed CSPRNG (aws-lc-rs).
pub struct OsSalt;
impl SaltSource for OsSalt {
    fn next_salt(&mut self, _: &SaltReq) -> [u8; 24] {
        let mut s = [0u8; 24];
        os_fill(&mut s).expect("rng");
        s
    }
}

/// A broken/replayed RNG: cycles through a fixed list. Models a VM snapshot
/// restore, a forked process with duplicated user-space RNG state, or a bug.
pub struct ReplaySalt {
    pub list: Vec<[u8; 24]>,
    pub i: usize,
}
impl SaltSource for ReplaySalt {
    fn next_salt(&mut self, _: &SaltReq) -> [u8; 24] {
        let s = self.list[self.i % self.list.len()];
        self.i += 1;
        s
    }
}

/// Proposed hedge: salt = trunc192(keyed_blake3(k_h, rng_salt || chunk_id || codec ||
/// level || payload)). Collisions then need the RNG to repeat *and* the encoded bytes
/// to be identical, in which case reuse is harmless (same message).
pub struct HedgedSalt<S: SaltSource> {
    pub inner: S,
    pub key: [u8; 32],
}
impl<S: SaltSource> SaltSource for HedgedSalt<S> {
    fn next_salt(&mut self, req: &SaltReq) -> [u8; 24] {
        let r = self.inner.next_salt(req);
        let mut h = blake3::Hasher::new_keyed(&self.key);
        h.update(&r);
        h.update(req.chunk_id);
        h.update(&[req.codec, req.level as u8]);
        h.update(req.payload);
        let mut out = [0u8; 24];
        h.finalize_xof().fill(&mut out);
        out
    }
}

#[derive(Debug)]
pub enum SealError {
    Tripwire(Tripwire),
    Crypto,
}

pub struct Writer<S: SaltSource> {
    pub profile: Profile,
    pub dek: [u8; 32],
    pub family: [u8; 16],
    pub dek_epoch: u32,
    pub kind: u8,
    pub salts: S,
    pub ledger: Option<Ledger>,
}

pub fn aad_for(profile: Profile, h: &Header, family: &[u8; 16], kind: u8) -> Vec<u8> {
    // ADR-11: "format version, codec, level, ulen and chunk_id".
    let mut a = Vec::with_capacity(96);
    a.push(h.ver);
    a.push(h.codec);
    a.push(h.level as u8);
    a.extend_from_slice(&h.ulen.to_le_bytes());
    if matches!(profile, Profile::Hardened | Profile::Fips) {
        // Proposal: bind kind, DEK epoch, family and id length as well.
        a.push(kind);
        a.extend_from_slice(&h.dek_epoch.to_le_bytes());
        a.extend_from_slice(family);
        a.push(h.chunk_id.len() as u8);
    }
    a.extend_from_slice(&h.chunk_id);
    a
}

impl<S: SaltSource> Writer<S> {
    pub fn new(
        profile: Profile,
        dek: [u8; 32],
        family: [u8; 16],
        dek_epoch: u32,
        salts: S,
    ) -> Self {
        Self {
            profile,
            dek,
            family,
            dek_epoch,
            kind: KIND_DATA,
            salts,
            ledger: None,
        }
    }

    pub fn with_ledger(mut self) -> Self {
        self.ledger = Some(Ledger::new());
        self
    }

    /// The only entry point the product should have: compress, draw a *fresh*
    /// salt, seal. The salt never leaves this function.
    pub fn seal(
        &mut self,
        chunk_id: &[u8; 32],
        chunk: &[u8],
        codec: Codec,
    ) -> Result<Extent, SealError> {
        let payload = codec.compress(chunk);
        let salt = self.salts.next_salt(&SaltReq {
            chunk_id,
            codec: codec as u8,
            level: codec.level(),
            payload: &payload,
        });
        self.seal_payload(chunk_id, chunk.len() as u32, codec, &payload, salt)
    }

    /// Anti-pattern entry point: seal a given encoded payload with a given
    /// salt. Models a retry/re-encode path that cached the extent header (and
    /// its salt) from an earlier attempt. Tests only.
    pub fn seal_payload(
        &mut self,
        chunk_id: &[u8; 32],
        ulen: u32,
        codec: Codec,
        payload: &[u8],
        salt: [u8; 24],
    ) -> Result<Extent, SealError> {
        let mut h = Header {
            ver: FORMAT_VER,
            codec: codec as u8,
            level: codec.level(),
            ulen,
            dek_epoch: self.dek_epoch,
            chunk_id: *chunk_id,
            salt,
            nonce: None,
        };
        let ctx = KeyCtx {
            dek: &self.dek,
            family: &self.family,
            dek_epoch: self.dek_epoch,
            kind: self.kind,
        };
        let enc = EncDesc {
            codec: h.codec,
            level: h.level,
            clen: payload.len() as u32,
        };
        let key = extent_key(self.profile, &ctx, chunk_id, &salt, &enc);
        let aad = aad_for(self.profile, &h, &self.family, self.kind);
        let mut body = payload.to_vec();
        let nonce = if self.profile == Profile::Fips {
            let k =
                RandomizedNonceKey::new(&AES_256_GCM, &key[..]).map_err(|_| SealError::Crypto)?;
            let n = k
                .seal_in_place_append_tag(Aad::from(&aad[..]), &mut body)
                .map_err(|_| SealError::Crypto)?;
            let mut nb = [0u8; 12];
            nb.copy_from_slice(n.as_ref());
            h.nonce = Some(nb);
            nb
        } else {
            let k = LessSafeKey::new(
                UnboundKey::new(&AES_256_GCM, &key[..]).map_err(|_| SealError::Crypto)?,
            );
            let nb = [0u8; 12]; // constant nonce: every key encrypts exactly one payload
            k.seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nb),
                Aad::from(&aad[..]),
                &mut body,
            )
            .map_err(|_| SealError::Crypto)?;
            nb
        };
        if let Some(l) = self.ledger.as_mut() {
            l.record(&key, &nonce, &aad, payload)
                .map_err(SealError::Tripwire)?;
        }
        Ok(Extent { h, body })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Pre-crypto checks: unknown version/codec, `chunk_id` differs from the map entry.
    Header,
    Tag,
    Ulen,
    Decompress(DecompressError),
    /// Decrypted and decompressed fine but the recomputed keyed id differs.
    ChunkId,
}

#[derive(Default, Debug)]
pub struct OpenTrace {
    pub tag_checked: bool,
    pub decompress_called: bool,
    pub id_recomputed: bool,
}

pub struct Reader {
    pub profile: Profile,
    pub dek: [u8; 32],
    pub family: [u8; 16],
    pub kind: u8,
    /// Volume chunk size: absolute cap for `ulen` and for decompressed output.
    pub max_ulen: usize,
}

impl Reader {
    /// Verify-before-use order (ADR-11 "Integrity"): header sanity -> AEAD tag ->
    /// bounded decompress -> recompute the keyed id and compare in constant time.
    pub fn open(
        &self,
        e: &Extent,
        map_chunk_id: &[u8; 32],
        rehash: &dyn Fn(&[u8]) -> [u8; 32],
        trace: &mut OpenTrace,
    ) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        if e.h.ver != FORMAT_VER
            || Codec::from_u8(e.h.codec).is_none()
            || &e.h.chunk_id != map_chunk_id
        {
            return Err(OpenError::Header);
        }
        let codec = Codec::from_u8(e.h.codec).unwrap();
        if e.body.len() < crate::TAG_LEN {
            return Err(OpenError::Header);
        }
        let ctx = KeyCtx {
            dek: &self.dek,
            family: &self.family,
            dek_epoch: e.h.dek_epoch,
            kind: self.kind,
        };
        let clen = (e.body.len() - crate::TAG_LEN) as u32;
        let enc = EncDesc {
            codec: e.h.codec,
            level: e.h.level,
            clen,
        };
        let key = extent_key(self.profile, &ctx, &e.h.chunk_id, &e.h.salt, &enc);
        let aad = aad_for(self.profile, &e.h, &self.family, self.kind);
        let mut buf = e.body.clone();
        trace.tag_checked = true;
        let pt_len = if self.profile == Profile::Fips {
            let k = RandomizedNonceKey::new(&AES_256_GCM, &key[..]).map_err(|_| OpenError::Tag)?;
            let nb = e.h.nonce.ok_or(OpenError::Header)?;
            k.open_in_place(
                Nonce::assume_unique_for_key(nb),
                Aad::from(&aad[..]),
                &mut buf,
            )
            .map_err(|_| OpenError::Tag)?
            .len()
        } else {
            let k = LessSafeKey::new(
                UnboundKey::new(&AES_256_GCM, &key[..]).map_err(|_| OpenError::Tag)?,
            );
            k.open_in_place(
                Nonce::assume_unique_for_key([0u8; 12]),
                Aad::from(&aad[..]),
                &mut buf,
            )
            .map_err(|_| OpenError::Tag)?
            .len()
        };
        buf.truncate(pt_len);
        if e.h.ulen as usize > self.max_ulen {
            return Err(OpenError::Ulen);
        }
        trace.decompress_called = true;
        let out = decompress_bounded(codec, &buf, e.h.ulen as usize, self.max_ulen)
            .map_err(OpenError::Decompress)?;
        let out = Zeroizing::new(out);
        trace.id_recomputed = true;
        let got = rehash(&out);
        // blake3::Hash's PartialEq is constant-time.
        if blake3::Hash::from(got) != blake3::Hash::from(*map_chunk_id) {
            return Err(OpenError::ChunkId);
        }
        Ok(out)
    }
}
