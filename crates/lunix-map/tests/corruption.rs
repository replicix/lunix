//! Mutation test: damaged node bytes, wrong hashes, out-of-range locations
//! and truncated packs give errors, never a panic or a wrong entry.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{plain, Fix, SplitMix, ID_LEN};
use lunix_format::node::{DataEntry, NodeLoc};
use lunix_format::{Codec, ExtentKind, Level};
use lunix_iface::crypto::{EncodingDesc, ExtentCodec, OpenExpect, OpenOutput};
use lunix_iface::map::{BlockMap, Cached, ChunkNo, NodeRef, NodeSource};
use lunix_iface::store::PackRead;
use lunix_iface::{Error, ErrorKind, Result};
use lunix_map::{MapConfig, RadixMap};
use lunix_testkit::strategies::data_entry;
use lunix_testkit::MemPacks;

const CHUNKS: u64 = 200_000;
const CASES: usize = 10_000;

#[derive(Clone, Copy, Debug)]
enum Mutation {
    /// XOR one byte of the stored extent.
    Flip { at: usize, mask: u8 },
    /// The extent loses its tail.
    Truncate { keep: usize },
    /// The pack read fails (a truncated pack file).
    PackTooShort,
    /// Another node's valid extent is served instead (a wrong hash).
    Swap { with: NodeLoc },
    /// The location points somewhere else in the pack, or past its end.
    Shift { delta: u32 },
    /// A byte of the plaintext changes and the extent is sealed again, so the
    /// tag verifies and only the hash chain can notice.
    Reseal { at: usize, mask: u8 },
}

struct MutSource {
    inner: MemPacks,
    codec: Arc<dyn ExtentCodec>,
    target: NodeLoc,
    m: Mutation,
}

impl NodeSource for MutSource {
    fn load_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        if loc != self.target {
            return self.inner.read_node(loc, out);
        }
        let mut raw = Vec::new();
        match self.m {
            Mutation::Flip { at, mask } => {
                self.inner.read_node(loc, &mut raw)?;
                let n = raw.len();
                raw[at % n] ^= mask;
            }
            Mutation::Truncate { keep } => {
                self.inner.read_node(loc, &mut raw)?;
                raw.truncate(keep % raw.len());
            }
            Mutation::PackTooShort => {
                return Err(Error::Corrupt {
                    what: "pack",
                    detail: "range outside pack",
                })
            }
            Mutation::Swap { with } => self.inner.read_node(with, &mut raw)?,
            Mutation::Shift { delta } => {
                let shifted = NodeLoc {
                    off: loc.off.wrapping_add(delta),
                    ..loc
                };
                self.inner.read_node(shifted, &mut raw)?;
            }
            Mutation::Reseal { at, mask } => {
                self.inner.read_node(loc, &mut raw)?;
                let expect = OpenExpect {
                    chunk_size: 65_536,
                    chunk_id: None,
                    id_prefix: None,
                    verify_id: false,
                    output: OpenOutput::Plaintext,
                };
                let mut body = Vec::new();
                let meta = self
                    .codec
                    .open(ExtentKind::Node, &raw, &expect, &mut body)?;
                let n = body.len();
                body[at % n] ^= mask;
                let desc = EncodingDesc {
                    codec: Codec::Raw,
                    level: Level(0),
                    ulen: u32::try_from(n).expect("small"),
                    dict_id: None,
                };
                raw.clear();
                self.codec
                    .seal(ExtentKind::Node, &meta.chunk_id, &desc, &body, &mut raw)?;
            }
        }
        out.extend_from_slice(&raw);
        Ok(())
    }
}

fn build(fanout: u16) -> (Fix, lunix_format::commit::MapRoot, BTreeMap<u64, DataEntry>) {
    let f = Fix::new(fanout, CHUNKS);
    let mut rng = SplitMix(11);
    let mut model = BTreeMap::new();
    let mut txn = f.map.begin_update().expect("begin");
    for _ in 0..400 {
        // Clusters, so that leaves hold several runs.
        let base = rng.below(CHUNKS - 64);
        for k in 0..rng.below(8) + 1 {
            let ci = base + k;
            let e = data_entry(ci, rng.below(3) as u32, ID_LEN);
            txn.set(ChunkNo(ci), e).expect("set");
            model.insert(ci, e);
        }
    }
    txn.commit().expect("commit");
    let root = f.seal().root;
    (f, root, model)
}

fn run(fanout: u16, seed: u64) {
    let (f, root, model) = build(fanout);
    let mut nodes: Vec<NodeRef> = Vec::new();
    f.map
        .walk_nodes(&root, &mut |n| nodes.push(*n))
        .expect("nodes");
    assert!(nodes.len() > 40, "{} nodes", nodes.len());
    let keys: Vec<u64> = model.keys().copied().collect();
    let mut rng = SplitMix(seed);
    let mut by_kind = [0usize; 6];

    for case in 0..CASES {
        let target = nodes[rng.below(nodes.len() as u64) as usize].loc;
        let (kind, m) = match case % 6 {
            0 => (
                0,
                Mutation::Flip {
                    at: rng.next() as usize,
                    mask: 1 << rng.below(8),
                },
            ),
            1 => (
                1,
                Mutation::Truncate {
                    keep: rng.next() as usize,
                },
            ),
            2 => (2, Mutation::PackTooShort),
            3 => {
                let mut with = nodes[rng.below(nodes.len() as u64) as usize].loc;
                while with == target {
                    with = nodes[rng.below(nodes.len() as u64) as usize].loc;
                }
                (3, Mutation::Swap { with })
            }
            4 => (
                4,
                Mutation::Shift {
                    delta: if rng.below(2) == 0 {
                        rng.below(4096) as u32 + 1
                    } else {
                        u32::MAX - rng.below(4096) as u32
                    },
                },
            ),
            _ => (
                5,
                Mutation::Reseal {
                    at: rng.next() as usize,
                    mask: 1 << rng.below(8),
                },
            ),
        };
        by_kind[kind] += 1;
        let src = Arc::new(MutSource {
            inner: f.packs.clone(),
            codec: plain(),
            target,
            m,
        });
        let map = RadixMap::open(plain(), src, &root, CHUNKS, MapConfig::new(fanout, ID_LEN))
            .expect("open is lazy");

        for _ in 0..24 {
            let ci = if rng.below(2) == 0 {
                keys[rng.below(keys.len() as u64) as usize]
            } else {
                rng.below(CHUNKS)
            };
            match map.get(ChunkNo(ci)) {
                Ok(v) => assert_eq!(v, model.get(&ci).copied(), "case {case} {m:?} ci {ci}"),
                Err(e) => assert!(
                    matches!(
                        e.kind(),
                        ErrorKind::Auth | ErrorKind::Corrupt | ErrorKind::NotFound
                    ),
                    "case {case} {m:?}: {e}"
                ),
            }
            match map.try_get_cached(ChunkNo(ci)) {
                Cached::Hit(e) => assert_eq!(Some(e), model.get(&ci).copied()),
                Cached::Hole => assert!(!model.contains_key(&ci)),
                Cached::Miss => {}
            }
        }

        // Every node is read by a full walk, so the damaged one must be found.
        let mut seen = 0u64;
        let r = map.walk_leaves(&root, &mut |ci, e| {
            seen += 1;
            assert_eq!(model.get(&ci.0), Some(e), "walk gave a wrong entry");
        });
        match r {
            Err(e) => assert!(
                matches!(
                    e.kind(),
                    ErrorKind::Auth | ErrorKind::Corrupt | ErrorKind::NotFound
                ),
                "case {case} {m:?}: {e}"
            ),
            Ok(()) => panic!("case {case}: {m:?} on {target:?} went unnoticed ({seen} entries)"),
        }
        // The node walk does not read leaves: it may succeed, but must not panic.
        let _ = map.walk_nodes(&root, &mut |_| {});
    }
    assert!(by_kind.iter().all(|&n| n >= CASES / 6));
}

#[test]
fn mutations_fail_cleanly_at_fanout_256() {
    run(256, 1);
}

#[test]
fn mutations_fail_cleanly_at_fanout_512() {
    run(512, 2);
}

#[test]
fn an_intact_source_passes_the_same_probes() {
    let (f, root, model) = build(256);
    let map = f.open_at(&root, CHUNKS);
    assert_eq!(common::leaves(&map, &root), model);
    for (ci, e) in &model {
        assert_eq!(map.get(ChunkNo(*ci)).expect("get"), Some(*e));
    }
}
