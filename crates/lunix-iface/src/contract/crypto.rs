//! Crypto contract tests: what `lunix-crypto` and the testkit's `PlainCodec`
//! both promise. See the [`crate::crypto`] docs.

use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::{
    ChunkId, Codec, CommitSeq, Epoch, ExtentKind, Level, ObjectKind, TimeMs, WriteSeq,
};

use crate::crypto::{CommitCodec, EncodingDesc, ExtentCodec, MetaCodec, OpenExpect, OpenOutput};
use crate::error::ErrorKind;
use crate::store::CommitStore;

const CHUNK: usize = 16 * 1024;
const VOLUME_HASH: [u8; 32] = [3; 32];

/// The codecs of a volume under test.
pub trait CryptoFixture {
    /// Extents.
    fn extents(&self) -> &dyn ExtentCodec;
    /// Metadata objects.
    fn meta(&self) -> &dyn MetaCodec;
    /// Commits.
    fn commits(&self) -> &dyn CommitCodec;
    /// An empty scratch commit store for the chain tests.
    fn store(&self) -> &dyn CommitStore;
    /// The codecs (with levels) the fixture can compress for; at least `Raw`.
    fn codecs(&self) -> Vec<(Codec, Level)>;
    /// Compresses `plain` with `codec` (the destage side of `seal`).
    fn compress(&self, codec: Codec, plain: &[u8]) -> Vec<u8>;
}

fn chunk(seed: u8) -> Vec<u8> {
    (0..CHUNK)
        .map(|i| (i as u8).wrapping_mul(7) ^ seed)
        .collect()
}

fn expect(id: Option<&ChunkId>, verify: bool, output: OpenOutput) -> OpenExpect<'_> {
    OpenExpect {
        chunk_size: CHUNK as u32,
        chunk_id: id,
        id_prefix: None,
        verify_id: verify,
        output,
    }
}

fn seal<F: CryptoFixture>(
    f: &F,
    codec: Codec,
    level: Level,
    data: &[u8],
) -> (ChunkId, Vec<u8>, Vec<u8>) {
    let id = f.extents().chunk_id(ExtentKind::Data, data);
    let comp = f.compress(codec, data);
    let desc = EncodingDesc {
        codec,
        level,
        ulen: data.len() as u32,
        dict_id: None,
    };
    let mut out = Vec::new();
    f.extents()
        .seal(ExtentKind::Data, &id, &desc, &comp, &mut out)
        .expect("seal");
    (id, comp, out)
}

/// Sealing then opening returns the plaintext (and the stored form) for every
/// codec the fixture offers, and the same for a map node.
pub fn extent_round_trip_per_codec<F: CryptoFixture>(f: &mut F) {
    let data = chunk(1);
    for (codec, level) in f.codecs() {
        let (id, comp, bytes) = seal(f, codec, level, &data);
        let mut out = Vec::new();
        let meta = f
            .extents()
            .open(
                ExtentKind::Data,
                &bytes,
                &expect(Some(&id), true, OpenOutput::Plaintext),
                &mut out,
            )
            .expect("open plaintext");
        assert_eq!(
            (meta.chunk_id, meta.desc.codec, meta.desc.ulen),
            (id, codec, CHUNK as u32)
        );
        assert_eq!(out, data, "{codec:?}");
        let mut stored = Vec::new();
        f.extents()
            .open(
                ExtentKind::Data,
                &bytes,
                &expect(Some(&id), true, OpenOutput::Stored),
                &mut stored,
            )
            .expect("open stored");
        assert_eq!(stored, comp, "{codec:?}");
        let mut appended = vec![9u8];
        f.extents()
            .open(
                ExtentKind::Data,
                &bytes,
                &expect(None, false, OpenOutput::Plaintext),
                &mut appended,
            )
            .expect("open appends");
        assert_eq!(&appended[..1], &[9], "open appends to the buffer");
    }
    let node = vec![0x42u8; 300];
    let nid = f.extents().chunk_id(ExtentKind::Node, &node);
    let desc = EncodingDesc {
        codec: Codec::Raw,
        level: Level(0),
        ulen: 300,
        dict_id: None,
    };
    let mut bytes = Vec::new();
    f.extents()
        .seal(ExtentKind::Node, &nid, &desc, &node, &mut bytes)
        .expect("seal node");
    let mut out = Vec::new();
    f.extents()
        .open(
            ExtentKind::Node,
            &bytes,
            &expect(Some(&nid), true, OpenOutput::Plaintext),
            &mut out,
        )
        .expect("open node");
    assert_eq!(out, node);
}

/// Flipping any single byte of an extent makes `open` fail and leaves the
/// output buffer untouched.
pub fn any_flipped_byte_fails<F: CryptoFixture>(f: &mut F) {
    let data = chunk(2);
    let (id, _, bytes) = seal(f, Codec::Raw, Level(0), &data);
    for i in 0..bytes.len() {
        let mut bad = bytes.clone();
        bad[i] ^= 0x01;
        let mut out = Vec::new();
        let r = f.extents().open(
            ExtentKind::Data,
            &bad,
            &expect(Some(&id), false, OpenOutput::Plaintext),
            &mut out,
        );
        assert!(r.is_err(), "flip at byte {i} was accepted");
        assert!(out.is_empty(), "output touched on error at byte {i}");
    }
    for cut in [0, 1, bytes.len() / 2, bytes.len() - 1] {
        let mut out = Vec::new();
        assert!(f
            .extents()
            .open(
                ExtentKind::Data,
                &bytes[..cut],
                &expect(Some(&id), false, OpenOutput::Plaintext),
                &mut out
            )
            .is_err());
    }
}

/// A different expected id, a different chunk size, and an extent whose id is
/// not the hash of its content (with `verify_id`) are all rejected.
pub fn wrong_expectations_fail<F: CryptoFixture>(f: &mut F) {
    let data = chunk(3);
    let (id, _, bytes) = seal(f, Codec::Raw, Level(0), &data);
    let mut other = id;
    other.0[31] ^= 1;
    let mut out = Vec::new();
    let e = f
        .extents()
        .open(
            ExtentKind::Data,
            &bytes,
            &expect(Some(&other), false, OpenOutput::Stored),
            &mut out,
        )
        .expect_err("wrong id");
    assert_eq!(e.kind(), ErrorKind::Auth);
    let mut e2 = expect(Some(&id), false, OpenOutput::Stored);
    e2.chunk_size = (CHUNK * 2) as u32;
    assert!(f
        .extents()
        .open(ExtentKind::Data, &bytes, &e2, &mut out)
        .is_err());
    // Sealed under an id that is not the content's: only verify_id notices.
    let wrong = f.extents().chunk_id(ExtentKind::Data, &chunk(4));
    let desc = EncodingDesc {
        codec: Codec::Raw,
        level: Level(0),
        ulen: CHUNK as u32,
        dict_id: None,
    };
    let mut forged = Vec::new();
    f.extents()
        .seal(ExtentKind::Data, &wrong, &desc, &data, &mut forged)
        .expect("seal");
    assert!(f
        .extents()
        .open(
            ExtentKind::Data,
            &forged,
            &expect(Some(&wrong), false, OpenOutput::Stored),
            &mut out
        )
        .is_ok());
    let e = f
        .extents()
        .open(
            ExtentKind::Data,
            &forged,
            &expect(Some(&wrong), true, OpenOutput::Stored),
            &mut out,
        )
        .expect_err("id is not the content hash");
    assert_eq!(e.kind(), ErrorKind::Auth);
}

/// The chunk id depends only on the content, not on the seal.
pub fn chunk_id_is_stable_across_seals<F: CryptoFixture>(f: &mut F) {
    let data = chunk(5);
    let (a, _, _) = seal(f, Codec::Raw, Level(0), &data);
    let (b, _, _) = seal(f, Codec::Raw, Level(0), &data);
    assert_eq!(a, b);
    assert_ne!(a, f.extents().chunk_id(ExtentKind::Data, &chunk(6)));
}

/// Metadata objects round-trip and are bound to their kind and name.
pub fn meta_objects_are_bound_to_kind_and_name<F: CryptoFixture>(f: &mut F) {
    let name = "segments/0000000000000001";
    let sealed = f
        .meta()
        .seal(ObjectKind::Footer, name, b"footer body")
        .expect("seal");
    assert_eq!(
        f.meta()
            .open(ObjectKind::Footer, name, &sealed)
            .expect("open"),
        b"footer body"
    );
    assert!(f
        .meta()
        .open(ObjectKind::Footer, "segments/0000000000000002", &sealed)
        .is_err());
    assert!(f.meta().open(ObjectKind::Gc, name, &sealed).is_err());
    for i in 0..sealed.len() {
        let mut bad = sealed.clone();
        bad[i] ^= 0x80;
        assert!(
            f.meta().open(ObjectKind::Footer, name, &bad).is_err(),
            "flip at {i}"
        );
    }
}

fn full(seq: u64, parent_hash: [u8; 32], epoch: u64) -> Commit {
    Commit::Full(Full {
        common: CommonHeader {
            kind: CommitKind::Full,
            commit_seq: CommitSeq(seq),
            epoch: Epoch(epoch),
            parent_hash,
            created_at: TimeMs(1000 + seq),
        },
        cut_seq: WriteSeq(seq),
        size: 1 << 30,
        chain_floor: CommitSeq(0),
        map_root: MapRoot::empty([9; 32]),
        heat: None,
        map_packs: Vec::new(),
        new_segments: Vec::new(),
        index_packs: Vec::new(),
    })
}

fn build<F: CryptoFixture>(f: &F, n: u64, epochs: &[u64]) {
    let mut parent = VOLUME_HASH;
    for seq in 0..=n {
        let epoch = epochs.get(seq as usize).copied().unwrap_or(0);
        let sealed = f
            .commits()
            .seal_commit(&full(seq, parent, epoch))
            .expect("seal");
        f.store().put(CommitSeq(seq), &sealed.bytes).expect("put");
        assert_eq!(sealed.hash, f.commits().object_hash(&sealed.bytes));
        parent = sealed.hash;
    }
}

/// A commit round-trips under its own sequence number and fails under another.
pub fn commit_round_trips_and_is_bound_to_its_seq<F: CryptoFixture>(f: &mut F) {
    let c = full(4, [1; 32], 0);
    let sealed = f.commits().seal_commit(&c).expect("seal");
    assert_eq!(
        f.commits()
            .open_commit(CommitSeq(4), &sealed.bytes)
            .expect("open"),
        c
    );
    assert!(f
        .commits()
        .open_commit(CommitSeq(5), &sealed.bytes)
        .is_err());
    for i in 0..sealed.bytes.len() {
        let mut bad = sealed.bytes.clone();
        bad[i] ^= 0x01;
        assert!(
            f.commits().open_commit(CommitSeq(4), &bad).is_err(),
            "flip at {i}"
        );
    }
}

/// A good chain verifies to genesis and to a floor; broken links, a decreasing
/// epoch, a wrong genesis hash and a floor above the tip are rejected.
pub fn verify_chain_accepts_good_and_rejects_broken<F: CryptoFixture>(f: &mut F) {
    build(f, 5, &[]);
    let tip = f
        .commits()
        .verify_chain(f.store(), CommitSeq(5), CommitSeq(0), &VOLUME_HASH)
        .expect("good chain");
    assert_eq!(tip.common().commit_seq, CommitSeq(5));
    f.commits()
        .verify_chain(f.store(), CommitSeq(5), CommitSeq(2), &VOLUME_HASH)
        .expect("floor");
    assert!(f
        .commits()
        .verify_chain(f.store(), CommitSeq(5), CommitSeq(0), &[4; 32])
        .is_err());
    assert_eq!(
        f.commits()
            .verify_chain(f.store(), CommitSeq(2), CommitSeq(3), &VOLUME_HASH)
            .expect_err("floor")
            .kind(),
        ErrorKind::Invalid
    );
}

/// A chain whose epoch decreases is rejected.
pub fn verify_chain_rejects_a_decreasing_epoch<F: CryptoFixture>(f: &mut F) {
    build(f, 1, &[5, 3]);
    assert!(f
        .commits()
        .verify_chain(f.store(), CommitSeq(1), CommitSeq(0), &VOLUME_HASH)
        .is_err());
}

/// A child's `parent_hash` is the hash of the parent object as stored, so a
/// different (even valid) parent object does not match it.
pub fn commit_parent_hash_binds_the_parent_object<F: CryptoFixture>(f: &mut F) {
    build(f, 2, &[]);
    let forged = f
        .commits()
        .seal_commit(&full(1, [0xee; 32], 0))
        .expect("seal");
    let genuine = f.store().get(CommitSeq(1)).expect("get");
    assert_ne!(
        f.commits().object_hash(&forged.bytes),
        f.commits().object_hash(&genuine)
    );
    let tip = f
        .commits()
        .open_commit(CommitSeq(2), &f.store().get(CommitSeq(2)).expect("get"))
        .expect("open");
    assert_eq!(tip.common().parent_hash, f.commits().object_hash(&genuine));
    assert_ne!(
        tip.common().parent_hash,
        f.commits().object_hash(&forged.bytes)
    );
}
