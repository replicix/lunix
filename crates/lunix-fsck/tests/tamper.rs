//! Tamper (R1-R4) on `integrity` and `encrypted` volumes (and plain where the
//! check does not need a key): each attack has a documented code.

mod common;

use common::*;
use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full, MapRoot};
use lunix_format::local::{LocalPinsFile, RollbackFloor};
use lunix_format::node::{Child, Interior, NodeLoc};
use lunix_format::segment::{ContainerKind, Trailer, TRAILER_LEN};
use lunix_format::{Codec, CommitSeq, CryptoMode, Epoch, ExtentKind, Level, TimeMs};
use lunix_fsck::code;
use lunix_iface::crypto::{CommitCodec, EncodingDesc, OpenExpect, OpenOutput};
use lunix_iface::store::{CommitStore, PackStore, SegmentStore, VolumeDir};

fn small(mode: CryptoMode) -> Built {
    let mut s = Spec::new(mode, 16 * 1024);
    s.chunks = 4096;
    s.blobs = 300;
    s.per_commit = 300;
    build(&s)
}

fn need(b: &Built, deep: bool, codes: &[&str], what: &str) {
    let r = b.fsck(&if deep { b.deep() } else { b.opts() });
    assert_eq!(
        r.exit_code(),
        2,
        "{what} [{}]\n{}",
        mode_name(b.spec.mode),
        r.render_text()
    );
    for c in codes {
        assert!(
            r.has(c),
            "{what} [{}]: missing {c}\n{}",
            mode_name(b.spec.mode),
            r.render_text()
        );
    }
}

fn keyed_modes() -> [CryptoMode; 3] {
    [
        CryptoMode::Integrity,
        CryptoMode::Encrypted,
        CryptoMode::Plain,
    ]
}

/// Two extents of the same length in the first segment that holds such a pair.
fn equal_pair(b: &Built) -> (String, usize, usize, usize) {
    let store = b.open_store();
    for seg in &b.known.segments {
        let f = store.footer(*seg).expect("footer");
        let mut by_len: std::collections::BTreeMap<u32, Vec<u32>> = Default::default();
        for e in &f.entries {
            by_len.entry(e.clen).or_default().push(e.off);
        }
        if let Some((clen, offs)) = by_len.iter().find(|(_, v)| v.len() >= 2) {
            let (a, c) = (offs[0] as usize, offs[1] as usize);
            return (b.seg_rel(*seg), a, c, b.ovh() + *clen as usize);
        }
    }
    panic!("no pair of extents of one length");
}

#[test]
fn two_valid_extents_swapped_are_caught_by_the_footer_and_by_the_id() {
    for mode in keyed_modes() {
        let b = small(mode);
        let (rel, a, c, len) = equal_pair(&b);
        let mut bytes = b.read(&rel);
        let (x, y) = (bytes[a..a + len].to_vec(), bytes[c..c + len].to_vec());
        bytes[a..a + len].copy_from_slice(&y);
        bytes[c..c + len].copy_from_slice(&x);
        b.write(&rel, &bytes);
        // Every tag is still valid; the footer scan and seg_hash see it.
        need(
            &b,
            false,
            &[code::SEG_HASH, code::FOOTER_ENTRIES],
            "swapped extents",
        );
        // --deep also names the id.
        need(
            &b,
            true,
            &[code::SEG_HASH, code::EXTENT_ID],
            "swapped extents, deep",
        );
        let r = b.fsck(&b.deep());
        assert!(
            !r.has(code::EXTENT_AUTH),
            "tags are valid\n{}",
            r.render_text()
        );
    }
}

#[test]
fn extents_rearranged_with_the_footer_untouched_fail_seg_hash() {
    for mode in keyed_modes() {
        let b = small(mode);
        let seg = b.known.segments[0];
        let store = b.open_store();
        let f = store.footer(seg).expect("footer");
        drop(store);
        let mut es: Vec<_> = f.entries.clone();
        es.sort_by_key(|e| e.off);
        let rel = b.seg_rel(seg);
        let mut bytes = b.read(&rel);
        // Swap the first two extents (different lengths in general): the
        // region has the same total length.
        let ovh = b.ovh();
        let (e0, e1) = (&es[0], &es[1]);
        let (l0, l1) = (ovh + e0.clen as usize, ovh + e1.clen as usize);
        let start = e0.off as usize;
        let first = bytes[start..start + l0].to_vec();
        let second = bytes[start + l0..start + l0 + l1].to_vec();
        let mut re = second;
        re.extend_from_slice(&first);
        bytes[start..start + l0 + l1].copy_from_slice(&re);
        b.write(&rel, &bytes);
        need(&b, false, &[code::SEG_HASH], "rearranged extents");
    }
}

#[test]
fn a_footer_from_another_segment_makes_the_container_absent() {
    for mode in keyed_modes() {
        let b = small(mode);
        assert!(b.known.segments.len() >= 2);
        let (a, other) = (b.known.segments[0], b.known.segments[1]);
        let abytes = b.read(&b.seg_rel(a));
        let obytes = b.read(&b.seg_rel(other));
        let at = Trailer::decode(&abytes[abytes.len() - TRAILER_LEN..]).expect("trailer a");
        let ot = Trailer::decode(&obytes[obytes.len() - TRAILER_LEN..]).expect("trailer o");
        let mut forged = abytes[..at.footer_off as usize].to_vec();
        forged.extend_from_slice(
            &obytes[ot.footer_off as usize..ot.footer_off as usize + ot.footer_len as usize],
        );
        Trailer {
            footer_off: at.footer_off,
            footer_len: ot.footer_len,
            extent_count: at.extent_count,
            object_id: a,
            container_kind: ContainerKind::Segment,
        }
        .encode(&mut forged)
        .expect("trailer");
        b.write(&b.seg_rel(a), &forged);
        need(&b, false, &[code::FOOTER_AUTH], "foreign footer");
        // The container counts as absent: its entries resolve nowhere.
        let r = b.fsck(&b.opts());
        assert!(r.has(code::MAP_ENTRY_UNRESOLVED) || r.has(code::FOOTER_AUTH));
    }
}

// ---------------------------------------------------------------------------
// R2: a map whose pack and commit are fully consistent but whose nodes are
// not what their parents say. The test owns the keys, so it can re-seal the
// pack and the commit, which an attacker without the key cannot; fsck must
// still catch it, because the parent's `child_hash` is the anchor.

#[derive(Clone, Copy, Debug)]
enum Attack {
    /// Child 0 and child 1 swap their locations.
    WrongPlace,
    /// Leaf 0 is stored with codec 1 (lz4).
    Codec1,
    /// Leaf 0's extent keeps its id but holds leaf 1's content.
    WrongContent,
}

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

fn seal_node(
    b: &Built,
    hash: &[u8; 32],
    codec: Codec,
    plain_len: usize,
    payload: &[u8],
) -> Vec<u8> {
    let mut ext = Vec::new();
    b.crypto
        .extents()
        .seal(
            ExtentKind::Node,
            &lunix_format::ChunkId(*hash),
            &EncodingDesc {
                codec,
                level: Level(0),
                ulen: plain_len as u32,
                dict_id: None,
            },
            payload,
            &mut ext,
        )
        .expect("seal node");
    ext
}

/// Appends a commit `tip + 1` whose root is a re-sealed copy of the tip's
/// root in a new pack, with the attack applied to its children.
fn attack_map(b: &Built, attack: Attack) {
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
        pack: tip.map_root.pack.expect("root pack"),
        off: tip.map_root.off,
        len: tip.map_root.len,
    };
    let root = Interior::decode(&open_node(b, root_loc), b.spec.fanout).expect("root");
    let ovh = b.ovh() as u32;
    let mut pw = store.open_pack().expect("pack");
    let mut kids: Vec<Child> = root.children.clone();
    let live: Vec<usize> = kids
        .iter()
        .enumerate()
        .filter(|(_, c)| c.loc.is_some())
        .map(|(i, _)| i)
        .collect();
    assert!(
        live.len()
            >= if matches!(attack, Attack::Codec1) {
                1
            } else {
                2
            },
        "need leaves"
    );
    let bodies: Vec<Vec<u8>> = live
        .iter()
        .map(|i| open_node(b, kids[*i].loc.expect("loc")))
        .collect();
    for (n, i) in live.iter().enumerate() {
        let hash = kids[*i].hash;
        let ext = match (attack, n) {
            (Attack::Codec1, 0) => {
                let comp = lz4_runs(&bodies[0]);
                assert!(comp.len() < bodies[0].len(), "the leaf must compress");
                seal_node(b, &hash, Codec::Lz4, bodies[0].len(), &comp)
            }
            (Attack::WrongContent, 0) => {
                seal_node(b, &hash, Codec::Raw, bodies[1].len(), &bodies[1])
            }
            _ => seal_node(b, &hash, Codec::Raw, bodies[n].len(), &bodies[n]),
        };
        let loc = pw.append_node(&hash, &ext).expect("append");
        kids[*i].loc = Some(loc);
    }
    if let Attack::WrongPlace = attack {
        let (a, c) = (live[0], live[1]);
        let la = kids[a].loc;
        kids[a].loc = kids[c].loc;
        kids[c].loc = la;
    }
    let new_root = Interior {
        level: root.level,
        children: kids,
    };
    let mut body = Vec::new();
    new_root.encode(&mut body).expect("encode root");
    let ext = seal_node(b, &tip.map_root.hash, Codec::Raw, body.len(), &body);
    let rloc = pw.append_node(&tip.map_root.hash, &ext).expect("root");
    let sealed = pw.seal().expect("seal pack");
    let seq = b.known.tip + 1;
    let _ = ovh;
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
            hash: tip.map_root.hash,
            pack: Some(sealed.pack),
            off: rloc.off,
            len: rloc.len,
        },
        heat: None,
        map_packs: vec![sealed.pack],
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
    let _ = ovh;
}

#[test]
fn a_consistent_map_that_is_not_what_its_parents_say_is_caught_by_r2() {
    for mode in keyed_modes() {
        for (attack, codes) in [
            (Attack::WrongPlace, vec![code::MAP_NODE_ID]),
            (Attack::Codec1, vec![code::MAP_NODE_CODEC]),
            (Attack::WrongContent, vec![code::MAP_NODE_HASH]),
        ] {
            let b = if matches!(attack, Attack::Codec1) {
                // One sparse leaf: its body has zero runs a run-length LZ4
                // block can shrink.
                let mut sp = Spec::new(mode, 16 * 1024);
                sp.chunks = 4096;
                sp.commits = 1;
                sp.per_commit = 3;
                sp.blobs = 3;
                build(&sp)
            } else {
                small(mode)
            };
            let clean = b.fsck(&b.opts());
            assert_eq!(clean.exit_code(), 0, "{}", clean.render_text());
            attack_map(&b, attack);
            need(&b, false, &codes, &format!("{attack:?}"));
        }
    }
}

// ---------------------------------------------------------------------------
// R4: the rollback floor.

fn set_floor(b: &Built, floor: Option<RollbackFloor>) {
    let mut f = LocalPinsFile::decode(&b.read("state/pins")).expect("pins");
    f.floor = floor;
    let mut out = Vec::new();
    f.encode(&mut out).expect("encode");
    b.write("state/pins", &out);
}

#[test]
fn a_chain_rolled_back_below_the_floor_is_rollback() {
    for mode in keyed_modes() {
        let b = small(mode);
        std::fs::remove_file(b.path(&b.commit_rel(b.known.tip))).expect("remove tip");
        need(&b, false, &[code::ROLLBACK], "rolled back");
        let r = b.fsck(&b.opts());
        assert!(!r.has(code::FORK));
    }
}

#[test]
fn another_commit_at_the_floor_height_is_a_fork() {
    for mode in keyed_modes() {
        let b = small(mode);
        // Same height, another hash: the floor remembers a different tip.
        set_floor(
            &b,
            Some(RollbackFloor {
                max_commit_seq: CommitSeq(b.known.tip),
                tip_hash: [9; 32],
            }),
        );
        need(&b, false, &[code::FORK], "fork at the floor height");
    }
}

#[test]
fn a_fork_two_commits_below_the_tip_is_caught_by_the_walk() {
    for mode in keyed_modes() {
        let b = small(mode);
        set_floor(
            &b,
            Some(RollbackFloor {
                max_commit_seq: CommitSeq(b.known.tip - 1),
                tip_hash: [9; 32],
            }),
        );
        need(&b, false, &[code::FORK], "fork below the tip");
        // Below chain_floor: the walk continues over commit files that GC
        // keeps for the floor; a missing one cannot be verified.
        set_floor(
            &b,
            Some(RollbackFloor {
                max_commit_seq: CommitSeq(0),
                tip_hash: [9; 32],
            }),
        );
        need(&b, false, &[code::FORK], "fork below chain_floor");
        std::fs::remove_file(b.path(&b.commit_rel(1))).expect("remove");
        need(
            &b,
            false,
            &[code::FORK_UNVERIFIABLE],
            "walk cannot complete",
        );
    }
}

#[test]
fn a_floor_above_the_tip_is_rollback_and_a_floor_at_the_tip_is_clean() {
    let b = small(CryptoMode::Integrity);
    set_floor(
        &b,
        Some(RollbackFloor {
            max_commit_seq: CommitSeq(b.known.tip + 5),
            tip_hash: [1; 32],
        }),
    );
    need(&b, false, &[code::ROLLBACK], "floor above the tip");
    set_floor(&b, None);
    let r = b.fsck(&b.opts());
    assert_eq!(r.exit_code(), 0, "{}", r.render_text());
    assert!(r.has(code::NO_FLOOR), "a note, not a finding of substance");
}

fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82f6_3b78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn a_version_1_pins_file_reads_without_a_floor() {
    for mode in keyed_modes() {
        let b = small(mode);
        let v2 = LocalPinsFile::decode(&b.read("state/pins")).expect("pins");
        let mut v1 = Vec::new();
        v1.extend_from_slice(b"LUNIXPIN");
        v1.extend_from_slice(&1u16.to_le_bytes());
        v1.push(u8::from(v2.pins.pin.is_some()));
        v1.push(v2.pins.pin.map_or(0, |p| p.mode as u8));
        v1.extend_from_slice(&[0; 4]);
        v1.extend_from_slice(&v2.pins.volume_uuid.0);
        v1.extend_from_slice(&v2.pins.family_id.0);
        v1.extend_from_slice(&v2.pins.max_keyring_seq.to_le_bytes());
        v1.extend_from_slice(&v2.pins.max_epoch.to_le_bytes());
        v1.extend_from_slice(&v2.pins.pin.map_or([0u8; 16], |p| p.fingerprint));
        v1.extend_from_slice(&[0; 12]);
        let crc = crc32c(&v1);
        v1.extend_from_slice(&crc.to_le_bytes());
        assert_eq!(v1.len(), LocalPinsFile::V1_LEN);
        b.write("state/pins", &v1);
        let r = b.fsck(&b.opts());
        assert_eq!(r.exit_code(), 0, "{}", r.render_text());
        assert!(r.has(code::NO_FLOOR));
    }
}
