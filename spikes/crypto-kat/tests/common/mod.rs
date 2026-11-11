#![allow(dead_code)]
use crypto_kat::codec::Codec;
use crypto_kat::extent::*;
use crypto_kat::kdf::*;

pub const FAMILY: [u8; 16] = [0x40; 16];
pub const KMK: [u8; 32] = [0x20; 32];

pub fn dek(epoch: u32) -> [u8; 32] {
    *purpose_key(&KMK, &FAMILY, Purpose::Dek { epoch })
}
pub fn addr_key() -> [u8; 32] {
    *purpose_key(&KMK, &FAMILY, Purpose::Addressing)
}
pub fn chunk_id(data: &[u8]) -> [u8; 32] {
    *blake3::keyed_hash(&addr_key(), data).as_bytes()
}
pub fn rehash(data: &[u8]) -> [u8; 32] {
    chunk_id(data)
}
pub fn data(len: usize, seed: u64) -> Vec<u8> {
    crypto_kat::attack::demo_chunk(len, seed)
}
pub fn writer(profile: Profile) -> Writer<OsSalt> {
    Writer::new(profile, dek(0), FAMILY, 0, OsSalt)
}
pub fn reader(profile: Profile) -> Reader {
    Reader {
        profile,
        dek: dek(0),
        family: FAMILY,
        kind: KIND_DATA,
        max_ulen: 65536,
    }
}
pub fn open(r: &Reader, e: &Extent, id: &[u8; 32]) -> (Result<Vec<u8>, OpenError>, OpenTrace) {
    let mut t = OpenTrace::default();
    let res = r.open(e, id, &rehash, &mut t).map(|z| z.to_vec());
    (res, t)
}
pub const ALL_CODECS: [Codec; 4] = [Codec::Raw, Codec::Lz4, Codec::Zstd1, Codec::Zstd3];
pub const ALL_PROFILES: [Profile; 3] = [Profile::Adr, Profile::Hardened, Profile::Fips];
