//! The worked examples of on-disk-formats.md section 23, reproduced with the
//! crate's own sealing: volume A (encrypted, KAT keys) and volume B
//! (integrity, IK `60..7f`): derived keys, descriptors, the plain and keyed
//! extent tags, genesis and commit 1, and the golden commits of every kind.

mod common;

use std::sync::Arc;

use common::*;
use lunix_crypto::{
    descriptor_mac, kmk_fingerprint, seal_descriptor, verify_descriptor_mac, MetaSuite, Root,
    VolumeCrypto, VolumeKeys,
};
use lunix_format::commit::decode_object;
use lunix_format::keyring::RootKind;
use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::{
    ChunkId, Codec, CommitSeq, CryptoMode, DekGen, ExtentKind, FamilyId, IdLen, Level, Profile,
    TimeMs, VolumeUuid,
};
use lunix_iface::crypto::{CommitCodec, EncodingDesc};
use lunix_iface::store::CommitStore;

macro_rules! golden {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../lunix-format/tests/golden/",
            $name
        ))
        .to_vec()
    };
}

const KAT: &str = include_str!("../../../bench/crypto-kat/raw/kat-vectors.json");

fn kat(name: &str) -> Vec<u8> {
    let m: std::collections::BTreeMap<String, String> = serde_json::from_str(KAT).expect("json");
    hex(&m[name])
}

fn descriptor_a() -> VolumeDescriptor {
    VolumeDescriptor {
        crypto_mode: CryptoMode::Encrypted,
        profile: Profile::Default,
        block_size: 4096,
        chunk_size: 65_536,
        map_fanout: 512,
        id_len: IdLen::ThirtyTwo,
        codec_policy: CodecPolicy::Auto,
        codec_level: 1,
        volume_uuid: VolumeUuid(arr(&hex("a0a1a2a3a4a546a788a9aaabacadaeaf"))),
        family_id: FamilyId(arr(&seq_bytes(0x40, 16))),
        kmk_fingerprint: [0; 16],
        created_at: TimeMs(1_794_441_600_000),
        lineage: Vec::new(),
        mac: [0; 32],
    }
}

fn descriptor_b_bytes() -> Vec<u8> {
    hex(
        "4c554e4958564f4c 0100 01 00 00100000 00400000 0002 10 02 00 00 000000000000
         b0b1b2b3b4b546b788b9babbbcbdbebf 505152535455565758595a5b5c5d5e5f
         92a4ec2689f62aab86bc8f278713183e 00dc0dcda1010000
         4951c7ffd237693e73cd15e17c6916869a540c098bc6fdbe90340ceba80801b5",
    )
}

fn ik_b() -> Root {
    Root::from_bytes(RootKind::Ik, &arr(&seq_bytes(0x60, 32)))
}

fn volume_b() -> (VolumeDescriptor, VolumeCrypto) {
    let desc = VolumeDescriptor::decode(&descriptor_b_bytes()).expect("descriptor B");
    let crypto = VolumeCrypto::new(&desc, Some(ik_b()), DekGen(0)).expect("crypto B");
    (desc, crypto)
}

#[test]
fn descriptor_a_mac_and_fingerprint_match_the_doc() {
    let kmk = Root::from_bytes(RootKind::Kmk, &arr(&seq_bytes(0x20, 32)));
    let mut d = descriptor_a();
    let bytes = seal_descriptor(&mut d, Some(&kmk)).expect("seal");
    assert_eq!(bytes.len(), 120);
    assert_eq!(
        d.kmk_fingerprint.to_vec(),
        kat("purpose_pin")[..16].to_vec()
    );
    assert_eq!(
        hex::encode(d.mac),
        "55921593482045805e11561ca0a5eb62\
         7077f183a61f8f6919cb571187809bf4"
    );
    assert!(verify_descriptor_mac(&d, Some(&kmk)));
    // A different root (or none) does not verify it.
    let other = Root::from_bytes(RootKind::Kmk, &root_bytes(1));
    assert!(!verify_descriptor_mac(&d, Some(&other)));
    assert!(!verify_descriptor_mac(&d, None));
    assert_eq!(VolumeDescriptor::decode(&bytes).expect("decode"), d);
}

#[test]
fn descriptor_b_and_its_derived_keys_match_the_doc() {
    let (desc, crypto) = volume_b();
    assert!(verify_descriptor_mac(&desc, Some(&ik_b())));
    assert_eq!(
        kmk_fingerprint(&desc, &ik_b()).expect("fp").to_vec(),
        hex("92a4ec2689f62aab86bc8f278713183e")
    );
    // Re-sealing the decoded descriptor reproduces the doc's bytes.
    let mut d = desc.clone();
    d.mac = [0; 32];
    d.kmk_fingerprint = [0; 16];
    assert_eq!(
        seal_descriptor(&mut d, Some(&ik_b())).expect("seal"),
        descriptor_b_bytes()
    );
    let keys = crypto.volume_keys();
    assert_eq!(
        hex::encode(keys.hmac_key_for_tests().expect("hmac")),
        "aa13505a19bd5e783cbc31582031cb918c1dfaa9aa6b94b27057acb06306f4df"
    );
    assert_eq!(
        hex::encode(keys.tag_key_for_tests().expect("tag")),
        "8f5a8aa5bd1c61e6d9b3e275436f35de2673144fc8d12cb7aab4b39398b09322"
    );
    let key = lunix_crypto::testing::purpose_key(
        Profile::Default,
        &ik_b(),
        &desc.family_id,
        &lunix_format::purpose::Purpose::DescriptorMac,
    )
    .expect("mac key");
    assert_eq!(
        hex::encode(&key[..]),
        "d56ef6bb95bf87c7b14ec9ad25b9eae3132d9120d815b04d6f467207f705d972"
    );
    let ia = lunix_crypto::testing::purpose_key(
        Profile::Default,
        &ik_b(),
        &desc.family_id,
        &lunix_format::purpose::Purpose::IntegrityAddressing,
    )
    .expect("integrity addressing key");
    assert_eq!(
        hex::encode(ia),
        "3e596debe98cf9c82d88e17c63d875e3f16dc9334a2dbe870f4dd827679372b8"
    );
    assert_eq!(keys.addressing_key_for_tests(false), Some(ia));
    // An IK has no data addressing key, no DEK and no metadata key.
    for p in [
        lunix_format::purpose::Purpose::DataAddressing,
        lunix_format::purpose::Purpose::Dek(DekGen(0)),
        lunix_format::purpose::Purpose::Metadata(lunix_format::ObjectKind::Commit),
        lunix_format::purpose::Purpose::NodeAddressing,
    ] {
        assert!(
            lunix_crypto::testing::purpose_key(Profile::Default, &ik_b(), &desc.family_id, &p)
                .is_err()
        );
    }
    // And a KMK has no integrity HMAC or extent tag key.
    let kmk = Root::from_bytes(RootKind::Kmk, &root_bytes(2));
    for p in [
        lunix_format::purpose::Purpose::IntegrityHmac,
        lunix_format::purpose::Purpose::ExtentTag,
        lunix_format::purpose::Purpose::IntegrityAddressing,
    ] {
        assert!(
            lunix_crypto::testing::purpose_key(Profile::Default, &kmk, &desc.family_id, &p)
                .is_err()
        );
    }
}

/// Section 23.2: the doc's keyed id, the tag of the doc and its unkeyed twin
/// on a plain volume.
#[test]
fn plain_and_keyed_tags_of_the_worked_extent() {
    // Keyed BLAKE3 under the integrity addressing key (purpose 0x0a).
    let id = ChunkId(arr(&hex(
        "8f6cf4e5e665b21ac12bd316397d883cb284a14b4999f9ee99301971d1b55ede",
    )));
    let plain_id = ChunkId(arr(&hex(
        "c6c4890119d834da404b94c707aa46a579d77f122211d145982c8ceb83e7f566",
    )));
    // The 75-byte LZ4 block of the doc, written out.
    let mut block = vec![0x1f, 0x41, 0x01, 0x00];
    block.extend(std::iter::repeat_n(0xff, 64));
    block.extend_from_slice(&[0x27, 0x50, 0x41, 0x41, 0x41, 0x41, 0x41]);
    assert_eq!(block.len(), 75);
    let data = vec![0x41u8; 16_384];
    assert_eq!(
        lz4_flex::block::decompress(&block, 16_384).expect("lz4"),
        data
    );
    let desc = EncodingDesc {
        codec: Codec::Lz4,
        level: Level(0),
        ulen: 16_384,
        dict_id: None,
    };

    // Volume B (integrity): the keyed id and the keyed tag of the doc.
    let (_, b) = volume_b();
    assert_eq!(
        hex::encode(
            b.volume_keys()
                .addressing_key_for_tests(false)
                .expect("0x0a")
        ),
        "3e596debe98cf9c82d88e17c63d875e3f16dc9334a2dbe870f4dd827679372b8"
    );
    assert_eq!(b.extents().chunk_id(ExtentKind::Data, &data), id);
    let mut out = Vec::new();
    b.extents()
        .seal(ExtentKind::Data, &id, &desc, &block, &mut out)
        .expect("seal");
    assert_eq!(out.len(), 143);
    assert_eq!(hex::encode(&out[127..]), "4e700344dd5d9ea60a34ba09c8a5f115");
    assert_eq!(&out[16..48], &id.0);
    // The whole extent equals the golden transcribed from the doc.
    assert_eq!(out, golden!("a0-extent-plain-keyed.bin"));

    // The same chunk on a plain volume: the unkeyed id and the unkeyed tag.
    let plain = volume(CryptoMode::Plain, 16_384, 3);
    assert_eq!(
        plain.crypto.extents().chunk_id(ExtentKind::Data, &data),
        plain_id
    );
    let mut out2 = Vec::new();
    plain
        .crypto
        .extents()
        .seal(ExtentKind::Data, &plain_id, &desc, &block, &mut out2)
        .expect("seal");
    assert_eq!(&out2[..16], &out[..16]);
    assert_eq!(&out2[16..48], &plain_id.0);
    assert_eq!(&out2[48..127], &out[48..127]);
    assert_eq!(
        hex::encode(&out2[127..]),
        "f21b8c973dcefd5e68954f1c2261587a"
    );
    assert_eq!(out2, golden!("03-extent-plain-unkeyed.bin"));
}

/// Section 23.5: the leaf, its node hash and its extent in the map pack, and
/// the data prefix inside it.
#[test]
fn the_worked_leaf_and_its_extent_are_reproduced() {
    let (_, b) = volume_b();
    let c = b.extents();
    let leaf = golden!("a0-node-leaf-one-entry.bin");
    assert_eq!(leaf.len(), 45);
    // The leaf's id column is the first 16 bytes of §23.2's keyed id.
    let data = vec![0x41u8; 16_384];
    let data_id = c.chunk_id(ExtentKind::Data, &data);
    assert_eq!(&leaf[29..], &data_id.0[..16]);
    // The node hash is unkeyed in integrity mode.
    let leaf_hash = c.chunk_id(ExtentKind::Node, &leaf);
    assert_eq!(
        hex::encode(leaf_hash.0),
        "dca03248122f117b77f7f5aeefbdd4867e0b4c958a051c8a245251324ef3b573"
    );
    let desc = EncodingDesc {
        codec: Codec::Raw,
        level: Level(0),
        ulen: 45,
        dict_id: None,
    };
    let mut ext = Vec::new();
    c.seal(ExtentKind::Node, &leaf_hash, &desc, &leaf, &mut ext)
        .expect("seal");
    assert_eq!(ext.len(), 113);
    assert_eq!(hex::encode(&ext[97..]), "26ff3ed4285b58271c9db42e836fb5b8");
    assert_eq!(ext, golden!("03-extent-node.bin"));
    let mut e = expect(0, Some(&leaf_hash));
    let mut got = Vec::new();
    c.open(ExtentKind::Node, &ext, &e, &mut got).expect("open");
    assert_eq!(got, leaf);

    // The root: header, child 0 = the leaf, 511 empty children.
    let root = golden!("05-node-interior-root.bin");
    assert_eq!(root.len(), 24_579);
    // An interior node's hash input is its children's hashes, not its stored
    // encoding (section 8.5), so the id comes from the doc, not from `chunk_id`.
    let root_hash = ChunkId(arr(&hex(
        "d126ea2c8ccdc011f08eeb71db94c59e659f9aba0229c8ea07bed39c2dfb5d0a",
    )));
    let mut rext = Vec::new();
    let rdesc = EncodingDesc {
        ulen: 24_579,
        ..desc
    };
    c.seal(ExtentKind::Node, &root_hash, &rdesc, &root, &mut rext)
        .expect("seal root");
    assert_eq!(rext.len(), 24_647);
    e = expect(0, Some(&root_hash));
    got.clear();
    c.open(ExtentKind::Node, &rext, &e, &mut got).expect("open");
    assert_eq!(got, root);

    // R1: the node extent presented as data never matches the data prefix.
    let prefix = lunix_format::IdPrefix::of(&data_id, IdLen::Sixteen);
    let mut as_data = expect(16_384, None);
    as_data.id_prefix = Some(&prefix);
    assert!(c
        .open(ExtentKind::Data, &ext, &as_data, &mut Vec::new())
        .is_err());
    // And the data extent of 23.2 presented as the leaf fails R2's hash.
    let data_ext = golden!("a0-extent-plain-keyed.bin");
    let wrong_node = expect(0, Some(&leaf_hash));
    assert!(c
        .open(ExtentKind::Node, &data_ext, &wrong_node, &mut Vec::new())
        .is_err());
}

fn expect<'a>(chunk: u32, id: Option<&'a ChunkId>) -> lunix_iface::crypto::OpenExpect<'a> {
    lunix_iface::crypto::OpenExpect {
        chunk_size: chunk,
        chunk_id: id,
        id_prefix: None,
        verify_id: false,
        output: lunix_iface::crypto::OpenOutput::Plaintext,
    }
}

#[test]
fn genesis_and_commit_one_of_volume_b_are_reproduced() {
    let (desc, crypto) = volume_b();
    let genesis = hex("4c584d31 01 01 00 00 a8000000
         01 01 0000 00000000
         0000000000000000 0000000000000000 0000000000000000
         6a6d952bbfcf690941f1c5659a312fd78d0faf46c82ecbfbdb1859ae9e51b8f5
         00dc0dcda1010000
         0000000000000000 0000004000000000 0000000000000000
         10c152fbfaaab0630db8b786eebf559cbd7b38cb964a20465d68e84412d61317
         0000000000000000 00000000 000000 00
         0000000000000000 00000000 00000000 00000000 00000000
         70776d1e8991acc35de81763bb936ddf0fc5d483fb50c57c906f86aa535c516e");
    let commit1 = hex("4c584d31 01 01 00 00 b8000000
         01 01 0100 00000000
         0100000000000000 0000000000000000 0000000000000000
         338e717684fbe16a5d22c7618a3d893a25d6808689df893eb6af53d942bb03c2
         30510ecda1010000
         0200000000000000 0000004000000000 0000000000000000
         d126ea2c8ccdc011f08eeb71db94c59e659f9aba0229c8ea07bed39c2dfb5d0a
         0200000000000000 71000000 476000 00
         0000000000000000 01000000 01000000 00000000 00000000
         0200000000000000 0100000000000000
         90291d1de8e5cebcf11f7b0b56d5c3c18925025604df373365cda4762166731d");
    let meta = crypto.meta();
    for (seq, bytes) in [(0u64, &genesis), (1, &commit1)] {
        // Opening authenticates the doc's bytes, sealing reproduces them.
        let c = meta.open_commit(CommitSeq(seq), bytes).expect("open");
        let sealed = meta.seal_commit(&c).expect("seal");
        assert_eq!(&sealed.bytes, bytes, "commit {seq}");
        assert_eq!(sealed.hash, *blake3::hash(bytes).as_bytes());
    }
    // The chain: genesis names the BLAKE3 of descriptor B.
    let store = MemCommits::new();
    store.put(CommitSeq(0), &genesis).expect("put");
    store.put(CommitSeq(1), &commit1).expect("put");
    let volume_hash = *blake3::hash(&descriptor_b_bytes()).as_bytes();
    let tip = meta
        .verify_chain(&store, CommitSeq(1), CommitSeq(0), &volume_hash)
        .expect("chain");
    assert_eq!(tip.common().commit_seq, CommitSeq(1));
    assert!(meta
        .verify_chain(&store, CommitSeq(1), CommitSeq(0), &[0; 32])
        .is_err());
    // Wrong volume or wrong seq fails the MAC.
    let other = volume(CryptoMode::Integrity, 16_384, 9);
    assert!(other
        .crypto
        .meta()
        .open_commit(CommitSeq(0), &genesis)
        .is_err());
    assert!(meta.open_commit(CommitSeq(1), &genesis).is_err());
    let _ = desc;
}

/// Golden commit objects of each non-FULL kind, MAC'd under volume B.
fn golden_commits() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("epoch", golden!("06-commit-epoch.bin")),
        ("epoch+authority", golden!("06-commit-epoch-authority.bin")),
        ("config", golden!("06-commit-config.bin")),
        ("config detached", golden!("06-commit-config-detached.bin")),
        ("delta", golden!("06-commit-delta.bin")),
        ("transfer", golden!("06-commit-transfer.bin")),
    ]
}

#[test]
fn every_golden_commit_kind_reproduces_its_hmac() {
    let (_, crypto) = volume_b();
    let meta: &MetaSuite = crypto.meta();
    for (name, bytes) in golden_commits() {
        let obj = decode_object(&bytes).expect("golden decodes");
        let seq = obj.commit.common().commit_seq;
        let c = meta.open_commit(seq, &bytes).unwrap_or_else(|e| {
            panic!("{name}: golden does not authenticate under volume B's key: {e}")
        });
        assert_eq!(meta.seal_commit(&c).expect("seal").bytes, bytes, "{name}");
    }
}

#[test]
fn a_shared_arc_is_the_same_keys() {
    let (_, crypto) = volume_b();
    let handle = crypto.keys();
    let again = VolumeKeys::from_handle(&handle).expect("ours");
    assert!(Arc::ptr_eq(&again, crypto.volume_keys()));
    assert_eq!(handle.mode(), CryptoMode::Integrity);
    assert_eq!(
        handle.fingerprint().to_vec(),
        hex("92a4ec2689f62aab86bc8f278713183e")
    );
    assert!(format!("{handle:?}").contains("redacted"));
    let _ = descriptor_mac;
}
