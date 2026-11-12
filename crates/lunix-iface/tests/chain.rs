//! The default `CommitCodec::verify_chain` over a trivial codec.

use std::collections::BTreeMap;
use std::sync::Mutex;

use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::{CommitSeq, CryptoMode, Epoch, ObjectKind, TimeMs, WriteSeq};
use lunix_iface::crypto::{CommitCodec, MetaCodec, SealedCommit};
use lunix_iface::store::{CommitInfo, CommitStore};
use lunix_iface::{Error, ErrorKind, Result};

struct Codec;

fn hash(b: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, x) in b.iter().enumerate() {
        out[i % 32] = out[i % 32].wrapping_mul(31).wrapping_add(*x).rotate_left(3);
    }
    out
}

impl MetaCodec for Codec {
    fn mode(&self) -> CryptoMode {
        CryptoMode::Plain
    }
    fn seal(&self, _k: ObjectKind, _n: &str, body: &[u8]) -> Result<Vec<u8>> {
        Ok(body.to_vec())
    }
    fn open(&self, _k: ObjectKind, _n: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        Ok(bytes.to_vec())
    }
}

impl CommitCodec for Codec {
    fn seal_commit(&self, commit: &Commit) -> Result<SealedCommit> {
        let bytes = commit.encode_object_form0_unsigned()?;
        // Unsigned object plus a marker byte: the trivial "MAC".
        let mut b = bytes;
        b.push(0x5a);
        let hash = self.object_hash(&b);
        Ok(SealedCommit { bytes: b, hash })
    }
    fn open_commit(&self, seq: CommitSeq, bytes: &[u8]) -> Result<Commit> {
        let (marker, rest) = bytes.split_last().ok_or(Error::Auth { what: "commit" })?;
        if *marker != 0x5a {
            return Err(Error::Auth { what: "commit" });
        }
        let mut full = rest.to_vec();
        full.extend_from_slice(&[0u8; 32]);
        let obj = lunix_format::commit::decode_object(&full)?;
        if obj.commit.common().commit_seq != seq {
            return Err(Error::Corrupt {
                what: "commit",
                detail: "wrong commit_seq",
            });
        }
        Ok(obj.commit)
    }
    fn object_hash(&self, bytes: &[u8]) -> [u8; 32] {
        hash(bytes)
    }
}

#[derive(Default)]
struct Store(Mutex<BTreeMap<u64, Vec<u8>>>);

impl CommitStore for Store {
    fn put(&self, seq: CommitSeq, o: &[u8]) -> Result<()> {
        self.0.lock().expect("lock").insert(seq.0, o.to_vec());
        Ok(())
    }
    fn get(&self, seq: CommitSeq) -> Result<Vec<u8>> {
        self.0
            .lock()
            .expect("lock")
            .get(&seq.0)
            .cloned()
            .ok_or(Error::not_found("commit"))
    }
    fn list(&self) -> Result<Vec<CommitInfo>> {
        Ok(Vec::new())
    }
    fn tip(&self) -> Result<Option<CommitSeq>> {
        Ok(None)
    }
    fn delete(&self, _s: CommitSeq, _f: CommitSeq) -> Result<u64> {
        Ok(0)
    }
    fn set_aside(&self, _s: CommitSeq) -> Result<()> {
        Ok(())
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

/// Builds genesis (seq 0) plus `n` children, returns the store.
fn build(n: u64, volume_hash: [u8; 32]) -> Store {
    let store = Store::default();
    let mut parent = volume_hash;
    for seq in 0..=n {
        let sealed = Codec.seal_commit(&full(seq, parent, 0)).expect("seal");
        store.put(CommitSeq(seq), &sealed.bytes).expect("put");
        parent = sealed.hash;
    }
    store
}

#[test]
fn a_good_chain_verifies_to_genesis_and_to_a_floor() {
    let vh = [4u8; 32];
    let store = build(5, vh);
    let tip = Codec
        .verify_chain(&store, CommitSeq(5), CommitSeq(0), &vh)
        .expect("to genesis");
    assert_eq!(tip.common().commit_seq, CommitSeq(5));
    // A floor above genesis ignores the (possibly absent) parent below it.
    store.0.lock().expect("lock").remove(&2);
    Codec
        .verify_chain(&store, CommitSeq(5), CommitSeq(3), &vh)
        .expect("to floor 3");
    let e = Codec
        .verify_chain(&store, CommitSeq(5), CommitSeq(0), &vh)
        .expect_err("missing link");
    assert_eq!(e.kind(), ErrorKind::NotFound);
}

#[test]
fn broken_links_are_rejected() {
    let vh = [4u8; 32];
    // Wrong genesis hash.
    let store = build(3, vh);
    assert!(Codec
        .verify_chain(&store, CommitSeq(3), CommitSeq(0), &[5u8; 32])
        .is_err());
    // A commit replaced by another valid commit with a different parent_hash.
    let store = build(3, vh);
    let forged = Codec.seal_commit(&full(2, [0xee; 32], 0)).expect("seal");
    store.0.lock().expect("lock").insert(2, forged.bytes);
    assert!(Codec
        .verify_chain(&store, CommitSeq(3), CommitSeq(0), &vh)
        .is_err());
    // A decreasing epoch.
    let store = Store::default();
    let mut parent = vh;
    for (seq, epoch) in [(0u64, 5u64), (1, 3)] {
        let s = Codec.seal_commit(&full(seq, parent, epoch)).expect("seal");
        store.put(CommitSeq(seq), &s.bytes).expect("put");
        parent = s.hash;
    }
    assert!(Codec
        .verify_chain(&store, CommitSeq(1), CommitSeq(0), &vh)
        .is_err());
    // Bad authentication and a floor above the tip.
    let store = build(2, vh);
    store
        .0
        .lock()
        .expect("lock")
        .get_mut(&1)
        .expect("c")
        .push(0);
    assert!(Codec
        .verify_chain(&store, CommitSeq(2), CommitSeq(0), &vh)
        .is_err());
    assert_eq!(
        Codec
            .verify_chain(&build(2, vh), CommitSeq(1), CommitSeq(2), &vh)
            .expect_err("floor")
            .kind(),
        ErrorKind::Invalid
    );
}
