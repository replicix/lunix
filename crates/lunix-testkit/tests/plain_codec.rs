//! `PlainCodec` beyond the contract suite: keys, modes, codecs, limits.

use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::{
    ChunkId, Codec, CommitSeq, CryptoMode, Epoch, ExtentKind, Level, ObjectKind, TimeMs, WriteSeq,
};
use lunix_iface::crypto::{
    CommitCodec, EncodingDesc, ExtentCodec, MetaCodec, OpenExpect, OpenOutput,
};
use lunix_iface::ErrorKind;
use lunix_testkit::PlainCodec;

const CHUNK: u32 = 16 * 1024;

fn desc(codec: Codec, ulen: u32) -> EncodingDesc {
    EncodingDesc {
        codec,
        level: Level(0),
        ulen,
        dict_id: None,
    }
}

fn expect(output: OpenOutput, verify_id: bool) -> OpenExpect<'static> {
    OpenExpect {
        chunk_size: CHUNK,
        chunk_id: None,
        id_prefix: None,
        verify_id,
        output,
    }
}

fn sealed(c: &PlainCodec, data: &[u8]) -> (ChunkId, Vec<u8>) {
    let id = c.chunk_id(ExtentKind::Data, data);
    let mut out = Vec::new();
    seal_ext(
        c,
        ExtentKind::Data,
        &id,
        &desc(Codec::Raw, data.len() as u32),
        data,
        &mut out,
    )
    .expect("seal");
    (id, out)
}

fn seal_ext(
    c: &PlainCodec,
    kind: ExtentKind,
    id: &ChunkId,
    d: &EncodingDesc,
    payload: &[u8],
    out: &mut Vec<u8>,
) -> lunix_iface::Result<()> {
    ExtentCodec::seal(c, kind, id, d, payload, out)
}

fn open_ext(
    c: &PlainCodec,
    kind: ExtentKind,
    bytes: &[u8],
    exp: &OpenExpect<'_>,
    out: &mut Vec<u8>,
) -> lunix_iface::Result<lunix_iface::crypto::OpenedMeta> {
    ExtentCodec::open(c, kind, bytes, exp, out)
}

fn seal_meta(c: &PlainCodec, kind: ObjectKind, name: &str, body: &[u8]) -> Vec<u8> {
    MetaCodec::seal(c, kind, name, body).expect("seal meta")
}

fn open_meta(
    c: &PlainCodec,
    kind: ObjectKind,
    name: &str,
    bytes: &[u8],
) -> lunix_iface::Result<Vec<u8>> {
    MetaCodec::open(c, kind, name, bytes)
}

fn full(seq: u64) -> Commit {
    Commit::Full(Full {
        common: CommonHeader {
            kind: CommitKind::Full,
            commit_seq: CommitSeq(seq),
            epoch: Epoch(0),
            parent_hash: [1; 32],
            created_at: TimeMs(5),
        },
        cut_seq: WriteSeq(1),
        size: 1 << 30,
        chain_floor: CommitSeq(0),
        map_root: MapRoot::empty([9; 32]),
        heat: None,
        map_packs: Vec::new(),
        new_segments: Vec::new(),
        index_packs: Vec::new(),
    })
}

#[test]
fn modes_and_layout() {
    assert_eq!(ExtentCodec::mode(&PlainCodec::new()), CryptoMode::Integrity);
    assert_eq!(MetaCodec::mode(&PlainCodec::unkeyed()), CryptoMode::Plain);
    let data = vec![7u8; CHUNK as usize];
    let (_, bytes) = sealed(&PlainCodec::new(), &data);
    assert_eq!(
        bytes.len(),
        data.len() + CryptoMode::Integrity.extent_ovh() as usize,
        "an extent is OVH + clen"
    );
}

#[test]
fn seal_is_deterministic_and_integrity_ids_are_keyed_for_data_only() {
    let data = vec![3u8; CHUNK as usize];
    let (a, x) = sealed(&PlainCodec::new(), &data);
    let (b, y) = sealed(&PlainCodec::new(), &data);
    assert_eq!((a, &x), (b, &y));
    let (c, z) = sealed(&PlainCodec::with_key([9; 16]), &data);
    assert_ne!(a, c, "the data id depends on the integrity key");
    assert_ne!(x, z);
    assert_ne!(
        a.0,
        *blake3::hash(&data).as_bytes(),
        "keyed, not the plain hash"
    );
    // Plain mode and every node hash are unkeyed BLAKE3 (D11, amendment).
    assert_eq!(
        PlainCodec::unkeyed().chunk_id(ExtentKind::Data, &data).0,
        *blake3::hash(&data).as_bytes()
    );
    for codec in [
        PlainCodec::new(),
        PlainCodec::with_key([9; 16]),
        PlainCodec::unkeyed(),
    ] {
        assert_eq!(
            codec.chunk_id(ExtentKind::Node, &data).0,
            *blake3::hash(&data).as_bytes()
        );
    }
}

#[test]
fn a_node_never_passes_as_data_under_the_integrity_key() {
    let c = PlainCodec::new();
    let body = vec![0x44u8; CHUNK as usize];
    let node_id = c.chunk_id(ExtentKind::Node, &body);
    assert_ne!(node_id, c.chunk_id(ExtentKind::Data, &body));
    // A raw extent carrying that node hash and the same bytes, opened as data
    // with verify_id, fails the key commitment.
    let mut ext = Vec::new();
    seal_ext(
        &c,
        ExtentKind::Data,
        &node_id,
        &desc(Codec::Raw, CHUNK),
        &body,
        &mut ext,
    )
    .unwrap();
    let mut out = Vec::new();
    let e = open_ext(
        &c,
        ExtentKind::Data,
        &ext,
        &expect(OpenOutput::Plaintext, true),
        &mut out,
    )
    .expect_err("node hash presented as a data id");
    assert_eq!(e.kind(), ErrorKind::Auth);
    // verify_id is ignored for nodes (the map checks the hash).
    let mut ext = Vec::new();
    seal_ext(
        &c,
        ExtentKind::Node,
        &node_id,
        &desc(Codec::Raw, CHUNK),
        &body,
        &mut ext,
    )
    .unwrap();
    open_ext(
        &c,
        ExtentKind::Node,
        &ext,
        &expect(OpenOutput::Plaintext, true),
        &mut Vec::new(),
    )
    .expect("node opens");
}

#[test]
fn a_dict_id_is_only_for_codec_3() {
    let c = PlainCodec::new();
    let id = ChunkId([1; 32]);
    let mut out = Vec::new();
    let with = EncodingDesc {
        dict_id: Some(4),
        ..desc(Codec::Zstd, 100)
    };
    assert_eq!(
        seal_ext(&c, ExtentKind::Data, &id, &with, &[0u8; 50], &mut out)
            .unwrap_err()
            .kind(),
        ErrorKind::Invalid
    );
    let without = EncodingDesc {
        dict_id: None,
        ..desc(Codec::ZstdDict, 100)
    };
    assert_eq!(
        seal_ext(&c, ExtentKind::Data, &id, &without, &[0u8; 50], &mut out)
            .unwrap_err()
            .kind(),
        ErrorKind::Invalid
    );
    assert!(out.is_empty());
}

#[test]
fn objects_do_not_open_under_another_key_or_mode() {
    let data = vec![5u8; CHUNK as usize];
    let (id, ext) = sealed(&PlainCodec::new(), &data);
    let mut exp = expect(OpenOutput::Plaintext, false);
    exp.chunk_id = Some(&id);
    for other in [PlainCodec::with_key([2; 16]), PlainCodec::unkeyed()] {
        let mut out = Vec::new();
        let e =
            open_ext(&other, ExtentKind::Data, &ext, &exp, &mut out).expect_err("foreign extent");
        assert_eq!(e.kind(), ErrorKind::Auth);
        let name = "segments/0000000000000001";
        let meta = seal_meta(&PlainCodec::new(), ObjectKind::Footer, name, b"body");
        let e = open_meta(&other, ObjectKind::Footer, name, &meta).expect_err("foreign meta");
        assert_eq!(e.kind(), ErrorKind::Auth);
        let sealed = PlainCodec::new().seal_commit(&full(3)).expect("seal");
        assert!(other.open_commit(CommitSeq(3), &sealed.bytes).is_err());
    }
}

#[test]
fn meta_moved_to_another_kind_is_an_auth_failure() {
    let c = PlainCodec::new();
    let name = "gc/0000000000000002";
    let sealed = seal_meta(&c, ObjectKind::Gc, name, b"x");
    let e = open_meta(&c, ObjectKind::Footer, name, &sealed).expect_err("kind");
    assert_eq!(e.kind(), ErrorKind::Auth);
    let e = MetaCodec::open(&c, ObjectKind::Gc, "gc/0000000000000003", &sealed).expect_err("name");
    assert_eq!(e.kind(), ErrorKind::Auth);
    assert_eq!(
        MetaCodec::open(&c, ObjectKind::Gc, name, &sealed).expect("open"),
        b"x"
    );
}

fn compress(codec: Codec, plain: &[u8]) -> Vec<u8> {
    match codec {
        Codec::Raw => plain.to_vec(),
        Codec::Lz4 => lz4_flex::block::compress(plain),
        Codec::Zstd => zstd::bulk::compress(plain, 3).expect("zstd"),
        Codec::ZstdDict => unreachable!("no dictionary codec"),
    }
}

fn seal_codec(c: &PlainCodec, codec: Codec, plain: &[u8]) -> (ChunkId, Vec<u8>) {
    let id = c.chunk_id(ExtentKind::Data, plain);
    let payload = compress(codec, plain);
    let mut ext = Vec::new();
    seal_ext(
        c,
        ExtentKind::Data,
        &id,
        &desc(codec, plain.len() as u32),
        &payload,
        &mut ext,
    )
    .expect("seal");
    (id, ext)
}

#[test]
fn lz4_and_zstd_open_to_plaintext_and_verify_the_id() {
    let plain: Vec<u8> = (0..CHUNK).map(|i| (i / 7) as u8).collect();
    for codec in [Codec::Raw, Codec::Lz4, Codec::Zstd] {
        let c = PlainCodec::new();
        let (id, ext) = seal_codec(&c, codec, &plain);
        let mut out = vec![1u8];
        let meta = open_ext(
            &c,
            ExtentKind::Data,
            &ext,
            &expect(OpenOutput::Plaintext, true),
            &mut out,
        )
        .expect("open");
        assert_eq!((meta.chunk_id, meta.desc.codec), (id, codec));
        assert_eq!(&out[1..], &plain[..], "{codec:?}: appended to out");
        // The stored form is the compressed payload, also with verify_id.
        let mut stored = Vec::new();
        open_ext(
            &c,
            ExtentKind::Data,
            &ext,
            &expect(OpenOutput::Stored, true),
            &mut stored,
        )
        .expect("stored");
        assert_eq!(stored, compress(codec, &plain));
    }
}

#[test]
fn a_payload_that_does_not_match_its_id_fails_verify_id_only() {
    let c = PlainCodec::new();
    let plain = vec![0x5au8; CHUNK as usize];
    for codec in [Codec::Raw, Codec::Lz4, Codec::Zstd] {
        let other = vec![0x5bu8; CHUNK as usize];
        let id = c.chunk_id(ExtentKind::Data, &plain);
        let mut ext = Vec::new();
        seal_ext(
            &c,
            ExtentKind::Data,
            &id,
            &desc(codec, CHUNK),
            &compress(codec, &other),
            &mut ext,
        )
        .expect("seal");
        let e = open_ext(
            &c,
            ExtentKind::Data,
            &ext,
            &expect(OpenOutput::Plaintext, true),
            &mut Vec::new(),
        )
        .expect_err("id mismatch");
        assert_eq!(e.kind(), ErrorKind::Auth, "{codec:?}");
        // Without verify_id the tag is the only check, and it holds.
        open_ext(
            &c,
            ExtentKind::Data,
            &ext,
            &expect(OpenOutput::Stored, false),
            &mut Vec::new(),
        )
        .expect("tag is valid");
    }
}

#[test]
fn undecodable_payloads_are_corrupt_and_the_dictionary_codec_is_unsupported() {
    let c = PlainCodec::new();
    let id = ChunkId([4; 32]);
    for codec in [Codec::Lz4, Codec::Zstd] {
        let mut ext = Vec::new();
        seal_ext(
            &c,
            ExtentKind::Data,
            &id,
            &desc(codec, CHUNK),
            &[0xffu8; 40],
            &mut ext,
        )
        .expect("seal");
        let mut out = Vec::new();
        let e = open_ext(
            &c,
            ExtentKind::Data,
            &ext,
            &expect(OpenOutput::Plaintext, false),
            &mut out,
        )
        .expect_err("garbage payload under a valid tag");
        assert_eq!(e.kind(), ErrorKind::Corrupt, "{codec:?}");
        assert!(out.is_empty());
    }
    // A frame that decodes to fewer bytes than the header's ulen.
    let mut ext = Vec::new();
    seal_ext(
        &c,
        ExtentKind::Data,
        &id,
        &desc(Codec::Zstd, CHUNK),
        &compress(Codec::Zstd, &[1u8; 100]),
        &mut ext,
    )
    .expect("seal");
    let e = open_ext(
        &c,
        ExtentKind::Data,
        &ext,
        &expect(OpenOutput::Plaintext, false),
        &mut Vec::new(),
    )
    .expect_err("length mismatch");
    assert_eq!(e.kind(), ErrorKind::Corrupt);
    // Codec 3 can be sealed and opened stored, never decoded.
    let mut ext = Vec::new();
    seal_ext(
        &c,
        ExtentKind::Data,
        &id,
        &EncodingDesc {
            dict_id: Some(1),
            ..desc(Codec::ZstdDict, CHUNK)
        },
        &[1u8; 30],
        &mut ext,
    )
    .expect("seal");
    let e = open_ext(
        &c,
        ExtentKind::Data,
        &ext,
        &expect(OpenOutput::Plaintext, false),
        &mut Vec::new(),
    )
    .expect_err("unsupported");
    assert_eq!(e.kind(), ErrorKind::Unsupported);
}

#[test]
fn decompression_is_capped_at_the_chunk_size() {
    let c = PlainCodec::new();
    let big = vec![9u8; 2 * CHUNK as usize];
    let id = c.chunk_id(ExtentKind::Data, &big);
    let mut ext = Vec::new();
    seal_ext(
        &c,
        ExtentKind::Data,
        &id,
        &desc(Codec::Zstd, 2 * CHUNK),
        &compress(Codec::Zstd, &big),
        &mut ext,
    )
    .expect("seal");
    assert!(open_ext(
        &c,
        ExtentKind::Data,
        &ext,
        &expect(OpenOutput::Plaintext, false),
        &mut Vec::new()
    )
    .is_err());
}

#[test]
fn seal_rejects_a_descriptor_that_does_not_fit_the_payload() {
    let c = PlainCodec::new();
    let id = ChunkId([1; 32]);
    let mut out = Vec::new();
    for (d, payload) in [
        (desc(Codec::Raw, 100), vec![0u8; 99]),
        (desc(Codec::Raw, 0), vec![]),
        (desc(Codec::Zstd, 10), vec![0u8; 11]),
    ] {
        let e = seal_ext(&c, ExtentKind::Data, &id, &d, &payload, &mut out)
            .expect_err("bad descriptor");
        assert_eq!(e.kind(), ErrorKind::Invalid);
    }
    assert!(out.is_empty(), "nothing appended on error");
}

#[test]
fn commits_bind_their_sequence_and_object_hash_is_stable() {
    let c = PlainCodec::new();
    let a = c.seal_commit(&full(7)).expect("seal");
    let b = c.seal_commit(&full(7)).expect("seal");
    assert_eq!(a, b);
    assert_eq!(a.hash, c.object_hash(&a.bytes));
    assert_ne!(a.hash, c.seal_commit(&full(8)).expect("seal").hash);
    assert_eq!(
        c.open_commit(CommitSeq(7), &a.bytes).expect("open"),
        full(7)
    );
    assert_eq!(
        c.open_commit(CommitSeq(8), &a.bytes)
            .expect_err("seq")
            .kind(),
        ErrorKind::Auth
    );
}

#[test]
fn node_extents_round_trip() {
    let c = PlainCodec::new();
    let node = vec![0x11u8; 5000];
    let id = c.chunk_id(ExtentKind::Node, &node);
    let mut ext = Vec::new();
    seal_ext(
        &c,
        ExtentKind::Node,
        &id,
        &desc(Codec::Raw, 5000),
        &node,
        &mut ext,
    )
    .expect("seal");
    let mut exp = expect(OpenOutput::Plaintext, true);
    exp.chunk_id = Some(&id);
    let mut out = Vec::new();
    open_ext(&c, ExtentKind::Node, &ext, &exp, &mut out).expect("open");
    assert_eq!(out, node);
    // A data extent does not open as a node and vice versa.
    assert!(open_ext(&c, ExtentKind::Data, &ext, &exp, &mut Vec::new()).is_err());
}
