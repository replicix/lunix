//! R2 (M2 security review): a node substituted for another valid node, from
//! the same pack or an older one, or stored with another codec, is refused.

mod common;

use std::sync::Arc;

use common::{plain, Fix, ID_LEN};
use lunix_format::node::NodeLoc;
use lunix_format::{ChunkId, Codec, ExtentKind, Level};
use lunix_iface::crypto::EncodingDesc;
use lunix_iface::map::{BlockMap, ChunkNo, NodeRef, NodeSource};
use lunix_iface::store::PackRead;
use lunix_iface::{ErrorKind, Result};
use lunix_map::RadixMap;
use lunix_testkit::strategies::data_entry;
use lunix_testkit::MemPacks;

/// Serves `serve` instead of the node at `at`.
struct Subst {
    inner: MemPacks,
    at: NodeLoc,
    serve: Vec<u8>,
}

impl NodeSource for Subst {
    fn load_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        if loc == self.at {
            out.extend_from_slice(&self.serve);
            return Ok(());
        }
        self.inner.read_node(loc, out)
    }
}

fn nodes(f: &Fix, root: &lunix_format::commit::MapRoot) -> Vec<NodeRef> {
    let mut v = Vec::new();
    f.map.walk_nodes(root, &mut |n| v.push(*n)).expect("walk");
    v
}

fn raw(f: &Fix, loc: NodeLoc) -> Vec<u8> {
    let mut b = Vec::new();
    f.packs.read_node(loc, &mut b).expect("read");
    b
}

fn expect_refused(f: &Fix, root: &lunix_format::commit::MapRoot, at: NodeLoc, serve: Vec<u8>) {
    expect_refused_with(f, root, at, serve, false);
}

/// `unsupported`: the fake codec cannot open codec 1 at all, which is also a
/// refusal (the real codec opens it and the codec-0 check refuses).
fn expect_refused_with(
    f: &Fix,
    root: &lunix_format::commit::MapRoot,
    at: NodeLoc,
    serve: Vec<u8>,
    unsupported: bool,
) {
    let src = Arc::new(Subst {
        inner: f.packs.clone(),
        at,
        serve,
    });
    let map = RadixMap::open(plain(), src, root, 5_000_000, f.cfg).expect("open");
    let err = map
        .walk_leaves(root, &mut |_, _| {})
        .expect_err("the substituted node must be refused");
    assert!(
        matches!(err.kind(), ErrorKind::Auth | ErrorKind::Corrupt)
            || (unsupported && err.kind() == ErrorKind::Unsupported),
        "{err}"
    );
    // Point lookups through the substituted node fail too, never return data.
    for ci in (0..5_000_000).step_by(499_979) {
        if let Ok(Some(e)) = map.get(ChunkNo(ci)) {
            assert_eq!(e, data_entry(ci, 1, ID_LEN), "a lookup returned wrong data");
        }
    }
}

#[test]
fn a_node_replaced_by_another_valid_node_is_refused() {
    let f = Fix::new(512, 5_000_000);
    common::set_all(&f.map, (0..40).map(|i| i * 123_457), 0);
    let r0 = f.seal().root;
    let old = nodes(&f, &r0);
    common::set_all(&f.map, (0..40).map(|i| i * 123_457), 1);
    let r1 = f.seal().root;
    let new = nodes(&f, &r1);
    assert_ne!(r0.pack, r1.pack);

    for n in &new {
        // Replay: the same position as of the older pack.
        let older = old
            .iter()
            .find(|o| o.level == n.level && o.loc.pack != n.loc.pack)
            .expect("an older node of that level");
        expect_refused(&f, &r1, n.loc, raw(&f, older.loc));
        // Substitution: a different node of the same pack.
        let other = new
            .iter()
            .find(|o| o.hash != n.hash && o.loc.pack == n.loc.pack)
            .expect("a sibling in the same pack");
        expect_refused(&f, &r1, n.loc, raw(&f, other.loc));
    }
}

#[test]
fn a_node_stored_with_another_codec_is_refused() {
    let f = Fix::new(512, 5_000_000);
    common::set_all(&f.map, [7, 3_000_000], 1);
    let root = f.seal().root;
    let n = nodes(&f, &root)[0];
    // A well-formed extent with the right id and tag but codec 1.
    let body = {
        let mut b = Vec::new();
        let e = lunix_iface::crypto::OpenExpect {
            chunk_size: 65_536,
            chunk_id: Some(&ChunkId(n.hash)),
            id_prefix: None,
            verify_id: false,
            output: lunix_iface::crypto::OpenOutput::Plaintext,
        };
        f.codec
            .open(ExtentKind::Node, &raw(&f, n.loc), &e, &mut b)
            .expect("open");
        b
    };
    let desc = EncodingDesc {
        codec: Codec::Lz4,
        level: Level(0),
        ulen: u32::try_from(body.len()).expect("small"),
        dict_id: None,
    };
    let mut ext = Vec::new();
    f.codec
        .seal(ExtentKind::Node, &ChunkId(n.hash), &desc, &body, &mut ext)
        .expect("seal");
    expect_refused_with(&f, &root, n.loc, ext, true);
}
