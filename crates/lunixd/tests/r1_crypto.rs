//! Rule R1 over the real codecs (M11 re-review: the testkit has no encrypted
//! codec, so the R1 cases run here over `lunix-crypto`): a swapped valid
//! extent, an older extent for the same LBA, a damaged tag and an extent at a
//! wrong offset each fail the read with EIO and count an
//! `extent_auth_failures` event, never returning data; a stale hint falls back
//! to the index and confirms the full id. Encrypted (AEAD) and integrity
//! (keyed tag) volumes, stored raw so that extents can be exchanged in place.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{pattern, scratch, Driver};
use lunix_crypto::Argon2Cost;
use lunix_format::commit::Commit;
use lunix_format::node::DataEntry;
use lunix_format::volume::CodecPolicy;
use lunix_format::{CryptoMode, ExtentKind, Hint, IdLen};
use lunix_iface::block::BlockEngine;
use lunix_iface::crypto::CommitCodec;
use lunix_iface::ctl::Secret;
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::{entry_hint, BlockMap, ChunkNo, NodeSource};
use lunix_iface::store::{CommitStore, SegmentStore};
use lunix_map::{MapConfig, RadixMap};
use lunixd::stack::{
    create_volume, open_volume, AttachCredential, CreateParams, CreatedVolume, OpenParams,
    VolumeStack,
};

const CS: u32 = 16 << 10;
const PASS: &[u8] = b"r1 passphrase";

fn create(parent: &Path, mode: CryptoMode) -> CreatedVolume {
    let tweak = |j: &mut lunix_format::local::VolumeJson| j.journal_max = 256 << 20;
    create_volume(&CreateParams {
        name: "r1",
        parent,
        size_bytes: 256 * u64::from(CS),
        chunk_size: CS,
        mode,
        policy: CodecPolicy::None,
        level: 0,
        id_len: IdLen::Sixteen,
        map_fanout: 512,
        key_file: None,
        passphrase: (mode == CryptoMode::Encrypted).then_some(PASS),
        argon2: Argon2Cost {
            m_kib: 19_456,
            t: 2,
            p: 1,
        },
        json: Some(&tweak),
    })
    .unwrap()
}

fn open(v: &CreatedVolume, mode: CryptoMode) -> VolumeStack {
    let mut p = OpenParams::new(v.voldir.clone(), 1);
    if mode == CryptoMode::Encrypted {
        p.credential = AttachCredential {
            passphrase: Some(Secret::new(PASS.to_vec())),
            ..AttachCredential::default()
        };
    }
    p.tweak = Some(Arc::new(|c| c.background = false));
    p.destage_workers = Some(1);
    open_volume(p).unwrap()
}

/// The tip's map entry of chunk `ci`.
fn entry(s: &VolumeStack, ci: u64) -> DataEntry {
    let tip = CommitStore::tip(&s.store).unwrap().unwrap();
    let Commit::Full(f) = s
        .crypto
        .meta()
        .open_commit(tip, &CommitStore::get(&s.store, tip).unwrap())
        .unwrap()
    else {
        panic!("not a FULL")
    };
    let source: Arc<dyn NodeSource> = Arc::new(s.store.clone());
    let m = RadixMap::open(
        Arc::clone(s.crypto.extents()),
        source,
        &f.map_root,
        f.size / u64::from(CS),
        MapConfig::new(512, IdLen::Sixteen),
    )
    .unwrap();
    m.get(ChunkNo(ci)).unwrap().expect("a stored chunk")
}

fn seg_path(voldir: &Path, h: &Hint) -> PathBuf {
    voldir
        .join("segments")
        .join(format!("{:016x}.seg", h.seg.raw()))
}

fn ext_len(s: &VolumeStack, h: &Hint) -> usize {
    (s.desc.crypto_mode.extent_ovh() + h.clen) as usize
}

fn read_ext(s: &VolumeStack, h: &Hint) -> Vec<u8> {
    let b = std::fs::read(seg_path(&s.voldir, h)).unwrap();
    b[h.off as usize..h.off as usize + ext_len(s, h)].to_vec()
}

fn write_ext(s: &VolumeStack, h: &Hint, bytes: &[u8]) {
    let p = seg_path(&s.voldir, h);
    let mut b = std::fs::read(&p).unwrap();
    assert_eq!(bytes.len(), ext_len(s, h), "same-size extents only");
    b[h.off as usize..h.off as usize + bytes.len()].copy_from_slice(bytes);
    std::fs::write(&p, &b).unwrap();
}

/// Chunks 0-3 in one commit, chunk 5 in a second (another segment); the
/// volume reopened, so caches are cold.
fn setup(t: &Path, mode: CryptoMode) -> (CreatedVolume, VolumeStack) {
    let v = create(t, mode);
    let s = open(&v, mode);
    let mut d = Driver::new(&s.engine, 1, 1 << 20);
    for c in 0..4u64 {
        assert!(d
            .write(c * u64::from(CS), &pattern(100 + c, CS as usize), false)
            .is_ok());
    }
    assert!(d.flush().is_ok());
    s.engine.commit_now(false).unwrap();
    assert!(d
        .write(5 * u64::from(CS), &pattern(105, CS as usize), false)
        .is_ok());
    assert!(d.flush().is_ok());
    s.engine.commit_now(false).unwrap();
    drop(d);
    s.engine.quiesce().unwrap();
    drop(s);
    let s = open(&v, mode);
    (v, s)
}

fn auth(s: &VolumeStack) -> u64 {
    s.engine.stats().extent_auth_failures
}

#[track_caller]
fn read_fails(s: &VolumeStack, ci: u64) {
    let mut d = Driver::new(&s.engine, 1, 1 << 20);
    let before = auth(s);
    let (st, data) = d.read_status(ci * u64::from(CS), CS);
    assert_eq!(st.errno(), Some(5), "EIO, never another chunk's bytes");
    assert!(data.is_empty());
    assert!(auth(s) > before, "counted as an authentication failure");
}

fn cases(mode: CryptoMode) {
    // A valid extent of another chunk (another segment) at chunk 0's hint.
    let Some(t) = scratch("r1") else { return };
    let (_v, s) = setup(t.path(), mode);
    let h0 = entry_hint(&entry(&s, 0));
    let h5 = entry_hint(&entry(&s, 5));
    assert_ne!(h0.seg, h5.seg);
    let e5 = read_ext(&s, &h5);
    write_ext(&s, &h0, &e5);
    read_fails(&s, 0);
    drop(s);

    // The same segment, another offset: chunk 1's extent at chunk 2's hint.
    let Some(t) = scratch("r1") else { return };
    let (_v, s) = setup(t.path(), mode);
    let h1 = entry_hint(&entry(&s, 1));
    let h2 = entry_hint(&entry(&s, 2));
    assert_eq!(h1.seg, h2.seg);
    let e1 = read_ext(&s, &h1);
    write_ext(&s, &h2, &e1);
    read_fails(&s, 2);
    drop(s);

    // A damaged tag.
    let Some(t) = scratch("r1") else { return };
    let (_v, s) = setup(t.path(), mode);
    let h3 = entry_hint(&entry(&s, 3));
    let mut e3 = read_ext(&s, &h3);
    let last = e3.len() - 1;
    e3[last] ^= 1;
    write_ext(&s, &h3, &e3);
    read_fails(&s, 3);
    drop(s);

    // An older extent of the same LBA.
    let Some(t) = scratch("r1") else { return };
    let (v, s) = setup(t.path(), mode);
    let old = entry_hint(&entry(&s, 0));
    let old_bytes = read_ext(&s, &old);
    let mut d = Driver::new(&s.engine, 1, 1 << 20);
    assert!(d.write(0, &pattern(999, CS as usize), false).is_ok());
    assert!(d.flush().is_ok());
    s.engine.commit_now(false).unwrap();
    drop(d);
    s.engine.quiesce().unwrap();
    drop(s);
    let s = open(&v, mode);
    let new = entry_hint(&entry(&s, 0));
    assert_ne!(old, new);
    write_ext(&s, &new, &old_bytes);
    read_fails(&s, 0);
}

#[test]
fn r1_cases_over_the_encrypted_codec() {
    cases(CryptoMode::Encrypted);
}

#[test]
fn r1_cases_over_the_integrity_codec() {
    cases(CryptoMode::Integrity);
}

/// A stale hint (the segment moved by a hand-made compaction, the index told
/// with `relocate`) falls back to the index and confirms the full id; a
/// missing segment is not an authentication failure.
fn stale_hint(mode: CryptoMode) {
    let Some(t) = scratch("r1s") else { return };
    let (_v, s) = setup(t.path(), mode);
    let e5 = entry(&s, 5);
    let old = entry_hint(&e5);
    let mut raw = Vec::new();
    SegmentStore::read_extent(&s.store, &old, &mut raw).unwrap();
    let data = pattern(105, CS as usize);
    let id = s.crypto.extents().chunk_id(ExtentKind::Data, &data);
    assert!(e5.prefix.matches(&id));
    let mut w = s.store.open_segment().unwrap();
    let new = w.append_extent(&id, None, &raw).unwrap();
    w.seal(None).unwrap();
    assert!(s.index.relocate(&id, &old, &new).unwrap());
    SegmentStore::delete(&s.store, old.seg).unwrap();
    let before = auth(&s);
    let mut d = Driver::new(&s.engine, 1, 1 << 20);
    let (st, got) = d.read_status(5 * u64::from(CS), CS);
    assert!(st.is_ok(), "{st:?}");
    assert!(got == data, "the relocated chunk reads back");
    assert_eq!(auth(&s), before, "a missing segment is not an auth failure");
}

#[test]
fn a_stale_hint_falls_back_to_the_index_encrypted() {
    stale_hint(CryptoMode::Encrypted);
}

#[test]
fn a_stale_hint_falls_back_to_the_index_integrity() {
    stale_hint(CryptoMode::Integrity);
}
