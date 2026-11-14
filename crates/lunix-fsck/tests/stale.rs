//! Stale hints: an entry whose hint no longer names its extent resolves
//! through the index, confirmed against the full id; `id_len = 0` volumes
//! have no such fallback; `--rebuild-index` restores the invariant.

mod common;

use std::collections::BTreeSet;

use common::*;
use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::local::RollbackFloor;
use lunix_format::node::{Child, Entry, Interior, Leaf, LeafParams, NodeLoc};
use lunix_format::{Codec, CommitSeq, Epoch, ExtentKind, IdLen, Level, TimeMs};
use lunix_fsck::{code, Options};
use lunix_iface::crypto::{CommitCodec, EncodingDesc, OpenExpect, OpenOutput};
use lunix_iface::index::ChunkIndex;
use lunix_iface::store::{CommitStore, PackStore, VolumeDir};
use lunix_index::LsmIndex;

fn open_node(b: &Built, loc: NodeLoc) -> Vec<u8> {
    let bytes = b.read(&b.pack_rel(loc.pack));
    let ext = &bytes[loc.off as usize..loc.off as usize + loc.len as usize];
    let mut out = Vec::new();
    b.crypto
        .extents()
        .open(
            ExtentKind::Node,
            ext,
            &OpenExpect {
                chunk_size: 65536,
                chunk_id: None,
                id_prefix: None,
                verify_id: false,
                output: OpenOutput::Plaintext,
            },
            &mut out,
        )
        .expect("open node");
    out
}

fn seal_node(b: &Built, hash: &[u8; 32], body: &[u8]) -> Vec<u8> {
    let mut ext = Vec::new();
    b.crypto
        .extents()
        .seal(
            ExtentKind::Node,
            &lunix_format::ChunkId(*hash),
            &EncodingDesc {
                codec: Codec::Raw,
                level: Level(0),
                ulen: body.len() as u32,
                dict_id: None,
            },
            body,
            &mut ext,
        )
        .expect("seal");
    ext
}

/// A new tip whose map is the old one with every entry passed through `f`
/// (chunk number, entry). The tree is two levels deep in these volumes.
fn restamp(b: &Built, f: impl Fn(u64, &mut lunix_format::node::DataEntry)) {
    let store = b.open_store();
    let tip_bytes = store.get(CommitSeq(b.known.tip)).expect("tip");
    let Commit::Full(tip) = b
        .crypto
        .meta()
        .open_commit(CommitSeq(b.known.tip), &tip_bytes)
        .expect("open")
    else {
        panic!("not full")
    };
    let root_loc = NodeLoc {
        pack: tip.map_root.pack.expect("pack"),
        off: tip.map_root.off,
        len: tip.map_root.len,
    };
    let fanout = b.spec.fanout;
    let root = Interior::decode(&open_node(b, root_loc), fanout).expect("root");
    let params = LeafParams {
        fanout,
        id_len: b.spec.id_len,
        ovh: b.ovh() as u32,
    };
    let mut pw = store.open_pack().expect("pack");
    let mut kids: Vec<Child> = root.children.clone();
    for (i, kid) in kids.iter_mut().enumerate() {
        let Some(loc) = kid.loc else { continue };
        let mut leaf = Leaf::decode(&open_node(b, loc), &params).expect("leaf");
        for (slot, e) in leaf.entries.iter_mut().enumerate() {
            if let Entry::Data(d) = e {
                f(i as u64 * u64::from(fanout) + slot as u64, d);
            }
        }
        let mut body = Vec::new();
        leaf.encode(&params, &mut body).expect("encode leaf");
        let hash = b.crypto.extents().chunk_id(ExtentKind::Node, &body).0;
        let ext = seal_node(b, &hash, &body);
        kid.hash = hash;
        kid.loc = Some(pw.append_node(&hash, &ext).expect("append"));
    }
    let new_root = Interior {
        level: root.level,
        children: kids,
    };
    let rhash = b
        .crypto
        .extents()
        .chunk_id(ExtentKind::Node, &new_root.hash_input().expect("input"))
        .0;
    let mut body = Vec::new();
    new_root.encode(&mut body).expect("encode");
    let rloc = pw
        .append_node(&rhash, &seal_node(b, &rhash, &body))
        .expect("root");
    let pack = pw.seal().expect("seal").pack;
    let seq = b.known.tip + 1;
    let full = Full {
        common: CommonHeader {
            kind: CommitKind::Full,
            commit_seq: CommitSeq(seq),
            epoch: Epoch(0),
            parent_hash: b.known.tip_hash,
            created_at: TimeMs(CREATED_AT + seq * 1000),
        },
        cut_seq: tip.cut_seq,
        size: tip.size,
        chain_floor: CommitSeq(
            tip.chain_floor
                .0
                .max(seq - u64::from(b.spec.retain_last - 1)),
        ),
        map_root: MapRoot {
            hash: rhash,
            pack: Some(pack),
            off: rloc.off,
            len: rloc.len,
        },
        heat: None,
        map_packs: vec![pack],
        new_segments: Vec::new(),
        index_packs: Vec::new(),
    };
    let sc = b
        .crypto
        .meta()
        .seal_commit(&Commit::Full(full))
        .expect("seal commit");
    store.put(CommitSeq(seq), &sc.bytes).expect("put");
    store
        .raise_rollback_floor(&RollbackFloor {
            max_commit_seq: CommitSeq(seq),
            tip_hash: sc.hash,
        })
        .expect("floor");
}

fn spec(mode: lunix_format::CryptoMode) -> Spec {
    let mut s = Spec::new(mode, 16 * 1024);
    s.chunks = 4096;
    s.blobs = 300;
    s.per_commit = 300;
    s
}

fn stale(_chunk: u64, d: &mut lunix_format::node::DataEntry) {
    d.off = d.off.wrapping_add(4);
}

fn index_entries(b: &Built) -> BTreeSet<(Vec<u8>, u64, u32, u32)> {
    let store = b.open_store();
    let idx = LsmIndex::open(&b.dir, store.ids(), b.spec.id_len).expect("index");
    let mut set = BTreeSet::new();
    idx.iter_entries(&mut |e| {
        set.insert((e.id.0.to_vec(), e.hint.seg.raw(), e.hint.off, e.hint.clen));
    })
    .expect("iter");
    set
}

fn clear_index(b: &Built) {
    for name in list(&b.dir, "index") {
        std::fs::remove_file(b.path(&format!("index/{name}"))).expect("remove");
    }
}

#[test]
fn stale_hints_resolve_through_the_index_in_every_mode() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let want = b.fsck(&b.opts()).ratios.expect("ratios");
        restamp(&b, stale);
        let r = b.fsck(&b.deep());
        assert_eq!(r.exit_code(), 0, "{}", r.render_text());
        assert_eq!(r.ratios.expect("ratios").mapped_chunks, want.mapped_chunks);
        assert_eq!(r.ratios.expect("ratios").unique_chunks, want.unique_chunks);
    }
}

#[test]
fn without_an_index_a_stale_hint_is_an_error_and_rebuild_index_repairs_it() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let before = index_entries(&b);
        assert!(!before.is_empty());
        restamp(&b, stale);
        clear_index(&b);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 2, "{}", r.render_text());
        assert!(r.has(code::MAP_ENTRY_UNRESOLVED), "{}", r.render_text());
        assert!(r.has(code::INDEX_MISSING));
        let fixed = b.fsck(&Options {
            rebuild_index: true,
            ..b.opts()
        });
        assert!(
            fixed.repairs.iter().any(|a| a.action == "rebuild_index"),
            "{}",
            fixed.render_text()
        );
        assert_eq!(fixed.exit_code(), 0, "{}", fixed.render_text());
        // The index equals the original, entry for entry.
        assert_eq!(index_entries(&b), before, "mode {}", mode_name(mode));
        // And every entry still resolves.
        assert_eq!(b.fsck(&b.deep()).exit_code(), 0);
    }
}

#[test]
fn after_deleting_index_the_index_comes_back_equal_and_clean() {
    for mode in ALL_MODES {
        let b = build(&spec(mode));
        let before = index_entries(&b);
        clear_index(&b);
        let r = b.fsck(&b.opts());
        assert!(r.has(code::INDEX_MISSING), "{}", r.render_text());
        assert_eq!(r.exit_code(), 1);
        let fixed = b.fsck(&Options {
            rebuild_index: true,
            ..b.opts()
        });
        assert_eq!(fixed.exit_code(), 0, "{}", fixed.render_text());
        assert_eq!(index_entries(&b), before);
    }
}

#[test]
fn a_lean_volume_has_no_index_fallback() {
    let mut s = spec(lunix_format::CryptoMode::Integrity);
    s.id_len = IdLen::Zero;
    let b = build(&s);
    assert_eq!(b.fsck(&b.deep()).exit_code(), 0);
    restamp(&b, stale);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 2, "{}", r.render_text());
    assert!(r.has(code::MAP_ENTRY_UNRESOLVED));
}
