//! Shared helpers for the testkit's integration tests.
#![allow(dead_code)]

/// A complete unencrypted data extent for `id` with `payload_len` raw bytes.
pub fn data_extent(id: &lunix_format::ChunkId, payload_len: usize) -> Vec<u8> {
    use lunix_format::{Codec, ExtentKind, Level};
    use lunix_iface::crypto::{EncodingDesc, ExtentCodec};
    let payload = vec![0x3cu8; payload_len];
    let desc = EncodingDesc {
        codec: Codec::Raw,
        level: Level(0),
        ulen: u32::try_from(payload_len).expect("small"),
        dict_id: None,
    };
    let mut out = Vec::new();
    ExtentCodec::seal(
        &lunix_testkit::PlainCodec::new(),
        ExtentKind::Data,
        id,
        &desc,
        &payload,
        &mut out,
    )
    .expect("seal");
    out
}
