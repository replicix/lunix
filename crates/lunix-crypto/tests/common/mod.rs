//! Shared helpers of the lunix-crypto tests.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Mutex;

use lunix_crypto::{Root, VolumeCrypto};
use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::keyring::RootKind;
use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::{
    Codec, CommitSeq, CryptoMode, DekGen, Epoch, FamilyId, IdLen, Profile, TimeMs, VolumeUuid,
    WriteSeq,
};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec};
use lunix_iface::store::{CommitInfo, CommitStore};
use lunix_iface::{Error, Result};

pub fn hex(s: &str) -> Vec<u8> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    hex::decode(clean).expect("hex")
}

pub fn seq_bytes(start: u8, n: usize) -> Vec<u8> {
    (0..n).map(|i| start.wrapping_add(i as u8)).collect()
}

pub fn arr<const N: usize>(v: &[u8]) -> [u8; N] {
    v.try_into().expect("array length")
}

/// Compressible bytes with some structure.
pub fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| ((i / 7) as u8).wrapping_mul(3) ^ seed ^ ((i / 4099) as u8))
        .collect()
}

/// Incompressible bytes (xorshift).
pub fn noise(len: usize, mut s: u64) -> Vec<u8> {
    s |= 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

pub fn compress(codec: Codec, data: &[u8]) -> Vec<u8> {
    match codec {
        Codec::Raw => data.to_vec(),
        Codec::Lz4 => lz4_flex::block::compress(data),
        Codec::Zstd | Codec::ZstdDict => zstd::bulk::compress(data, 3).expect("zstd"),
    }
}

pub fn level_of(codec: Codec) -> lunix_format::Level {
    match codec {
        Codec::Raw | Codec::Lz4 => lunix_format::Level(0),
        Codec::Zstd | Codec::ZstdDict => lunix_format::Level(3),
    }
}

pub fn root_bytes(seed: u8) -> [u8; 32] {
    let mut b = [0u8; 32];
    for (i, x) in b.iter_mut().enumerate() {
        *x = seed.wrapping_add((i as u8).wrapping_mul(13)) ^ 0xa5;
    }
    b
}

pub fn descriptor(
    mode: CryptoMode,
    profile: Profile,
    chunk_size: u32,
    id_len: IdLen,
    family: [u8; 16],
    volume: [u8; 16],
) -> VolumeDescriptor {
    VolumeDescriptor {
        crypto_mode: mode,
        profile,
        block_size: 4096,
        chunk_size,
        map_fanout: 512,
        id_len,
        codec_policy: CodecPolicy::Auto,
        codec_level: 1,
        volume_uuid: VolumeUuid(volume),
        family_id: FamilyId(family),
        kmk_fingerprint: [0; 16],
        created_at: TimeMs(1_794_441_600_000),
        lineage: Vec::new(),
        mac: [0; 32],
    }
}

pub fn root_for(mode: CryptoMode, seed: u8) -> Option<Root> {
    match mode {
        CryptoMode::Plain => None,
        CryptoMode::Integrity => Some(Root::from_bytes(RootKind::Ik, &root_bytes(seed))),
        CryptoMode::Encrypted => Some(Root::from_bytes(RootKind::Kmk, &root_bytes(seed))),
    }
}

/// A volume with deterministic roots: `seed` picks root, family and uuid.
pub struct Vol {
    pub desc: VolumeDescriptor,
    pub crypto: VolumeCrypto,
}

pub fn volume_with(
    mode: CryptoMode,
    chunk_size: u32,
    id_len: IdLen,
    seed: u8,
    dek_gen: DekGen,
) -> Vol {
    let desc = descriptor(
        mode,
        Profile::Default,
        chunk_size,
        id_len,
        [seed; 16],
        [seed ^ 0x55; 16],
    );
    let crypto = VolumeCrypto::new(&desc, root_for(mode, seed), dek_gen).expect("volume keys");
    Vol { desc, crypto }
}

pub fn volume(mode: CryptoMode, chunk_size: u32, seed: u8) -> Vol {
    volume_with(mode, chunk_size, IdLen::ThirtyTwo, seed, DekGen(0))
}

pub const MODES: [CryptoMode; 3] = [
    CryptoMode::Encrypted,
    CryptoMode::Integrity,
    CryptoMode::Plain,
];

pub fn enc_desc(codec: Codec, ulen: usize) -> EncodingDesc {
    EncodingDesc {
        codec,
        level: level_of(codec),
        ulen: u32::try_from(ulen).expect("ulen"),
        dict_id: None,
    }
}

/// Seals `data` as a data extent with `codec`.
pub fn seal_data(
    codec_impl: &dyn ExtentCodec,
    codec: Codec,
    data: &[u8],
) -> (lunix_format::ChunkId, Vec<u8>, Vec<u8>) {
    let id = codec_impl.chunk_id(lunix_format::ExtentKind::Data, data);
    let comp = compress(codec, data);
    let mut out = Vec::new();
    codec_impl
        .seal(
            lunix_format::ExtentKind::Data,
            &id,
            &enc_desc(codec, data.len()),
            &comp,
            &mut out,
        )
        .expect("seal");
    (id, comp, out)
}

/// An in-memory commit store.
#[derive(Default)]
pub struct MemCommits(Mutex<BTreeMap<u64, Vec<u8>>>);

impl MemCommits {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CommitStore for MemCommits {
    fn put(&self, seq: CommitSeq, object: &[u8]) -> Result<()> {
        let mut m = self.0.lock().expect("lock");
        if m.contains_key(&seq.0) {
            return Err(Error::AlreadyExists { what: "commit" });
        }
        m.insert(seq.0, object.to_vec());
        Ok(())
    }

    fn get(&self, seq: CommitSeq) -> Result<Vec<u8>> {
        self.0
            .lock()
            .expect("lock")
            .get(&seq.0)
            .cloned()
            .ok_or(Error::NotFound { what: "commit" })
    }

    fn list(&self) -> Result<Vec<CommitInfo>> {
        Ok(self
            .0
            .lock()
            .expect("lock")
            .iter()
            .map(|(s, b)| CommitInfo {
                seq: CommitSeq(*s),
                size_bytes: b.len() as u64,
            })
            .collect())
    }

    fn tip(&self) -> Result<Option<CommitSeq>> {
        Ok(self
            .0
            .lock()
            .expect("lock")
            .keys()
            .next_back()
            .map(|s| CommitSeq(*s)))
    }

    fn delete(&self, _seq: CommitSeq, _floor: CommitSeq) -> Result<u64> {
        Err(Error::Refused {
            why: "test store keeps every commit",
        })
    }

    fn set_aside(&self, _seq: CommitSeq) -> Result<()> {
        Err(Error::Refused {
            why: "test store keeps every commit",
        })
    }
}

/// A FULL commit with the given parent hash and epoch.
pub fn full(seq: u64, parent_hash: [u8; 32], epoch: u64, salt: u64) -> Commit {
    Commit::Full(Full {
        common: CommonHeader {
            kind: CommitKind::Full,
            commit_seq: CommitSeq(seq),
            epoch: Epoch(epoch),
            parent_hash,
            created_at: TimeMs(1000 + seq + salt),
        },
        cut_seq: WriteSeq(seq + salt),
        size: 1 << 30,
        chain_floor: CommitSeq(0),
        map_root: MapRoot::empty([9; 32]),
        heat: None,
        map_packs: Vec::new(),
        new_segments: Vec::new(),
        index_packs: Vec::new(),
    })
}
