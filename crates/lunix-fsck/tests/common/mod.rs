//! A test volume builder over the real crates: `lunix-crypto` (keys, codecs,
//! commits), `lunix-store` (volume directory, segments, packs, commits,
//! checkpoint), `lunix-map` (the radix map) and `lunix-index`, plus journal
//! ring files written with the frozen format types. Nothing here is faked.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lunix_crypto::keyring::{self, KeyFile, NewSlot};
use lunix_crypto::{seal_descriptor, VolumeCrypto};
use lunix_format::commit::{Commit, CommitKind, CommonHeader, Full};
use lunix_format::journal::{BatchBuilder, FileHeader};
use lunix_format::keyring::RootKind;
use lunix_format::local::{CheckpointSlot, VolumeJson};
use lunix_format::node::{empty_hashes, levels_for, DataEntry};
use lunix_format::volume::{CodecPolicy, VolumeDescriptor};
use lunix_format::{
    ChunkId, Codec, CommitSeq, CryptoMode, DekGen, Epoch, ExtentKind, FamilyId, IdLen, IdPrefix,
    Level, LineageIdx, ObjId, Profile, TimeMs, VolumeUuid, WriteSeq,
};
use lunix_fsck::{check, Options, Report};
use lunix_iface::crypto::{CommitCodec, EncodingDesc, MetaCodec};
use lunix_iface::index::ChunkIndex;
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::map::{BlockMap, ChunkNo, NodeSource};
use lunix_iface::store::{
    CheckpointStore, CommitStore, GcStateStore, PackStore, SegmentStore, VolumeDir,
};
use lunix_index::LsmIndex;
use lunix_map::{MapConfig, RadixMap};
use lunix_store::{LocalVolume, StoreOptions};

pub const CREATED_AT: u64 = 1_794_441_600_000;

/// What to build.
#[derive(Clone, Debug)]
pub struct Spec {
    pub mode: CryptoMode,
    pub chunk_size: u32,
    pub fanout: u16,
    pub id_len: IdLen,
    pub retain_last: u32,
    /// Chunks the volume has.
    pub chunks: u64,
    /// Distinct chunk contents.
    pub blobs: usize,
    /// Mapped chunks of the first commit; each later commit adds as many
    /// again, overwrites a few and discards a few.
    pub per_commit: u64,
    pub commits: u64,
    /// Journal records written (`write_seq` 1 ..= this).
    pub journal_records: u64,
    pub lanes: u16,
    /// Segment target in bytes (small values make many segments).
    pub segment_target: u64,
    /// The last commit's `created_at` steps back (a clock step).
    pub clock_back: bool,
}

impl Spec {
    pub fn new(mode: CryptoMode, chunk_size: u32) -> Self {
        Self {
            mode,
            chunk_size,
            fanout: 512,
            id_len: IdLen::Sixteen,
            retain_last: 2,
            chunks: 8192,
            blobs: 700,
            per_commit: 700,
            commits: 3,
            journal_records: 330,
            lanes: 2,
            segment_target: 8 << 20,
            clock_back: false,
        }
    }
}

/// What the builder knows about the tip's content.
#[derive(Clone, Debug, Default)]
pub struct Known {
    pub tip: u64,
    pub tip_hash: [u8; 32],
    pub cut_seq: u64,
    pub mapped: u64,
    pub holes: u64,
    pub unique: u64,
    pub unique_ulen: u64,
    pub unique_clen: u64,
    pub segments: Vec<ObjId>,
    pub packs: Vec<ObjId>,
    pub runs: Vec<ObjId>,
    /// The content of the map at the tip: chunk to blob.
    pub map: BTreeMap<u64, usize>,
}

impl Known {
    pub fn dedup_ratio(&self) -> f64 {
        self.mapped as f64 / self.unique as f64
    }
    pub fn compression_ratio(&self) -> f64 {
        self.unique_ulen as f64 / self.unique_clen as f64
    }
}

pub struct Built {
    pub tmp: tempfile::TempDir,
    pub dir: PathBuf,
    pub spec: Spec,
    pub known: Known,
    pub desc: VolumeDescriptor,
    pub desc_bytes: Vec<u8>,
    pub crypto: VolumeCrypto,
    /// Key file of a keyed volume.
    pub key_file: Option<PathBuf>,
    pub keyring_path: Option<PathBuf>,
}

pub fn lcg(seed: u64, len: usize) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

fn put_len(out: &mut Vec<u8>, mut v: usize) {
    while v >= 255 {
        out.push(255);
        v -= 255;
    }
    out.push(v as u8);
}

/// A valid LZ4 block that compresses runs of one byte (offset-1 matches); the
/// workspace tests have no LZ4 encoder of their own.
pub fn lz4_runs(data: &[u8]) -> Vec<u8> {
    let n = data.len();
    let mut out = Vec::new();
    let mut lit_start = 0;
    let mut i = 1;
    while i + 12 <= n {
        let b = data[i - 1];
        let mut r = 0;
        while i + r + 5 < n && data[i + r] == b {
            r += 1;
        }
        if r >= 8 {
            let lits = &data[lit_start..i];
            let m = r - 4;
            out.push(((lits.len().min(15) as u8) << 4) | (m.min(15) as u8));
            if lits.len() >= 15 {
                put_len(&mut out, lits.len() - 15);
            }
            out.extend_from_slice(lits);
            out.extend_from_slice(&1u16.to_le_bytes());
            if m >= 15 {
                put_len(&mut out, m - 15);
            }
            i += r;
            lit_start = i;
        } else {
            i += 1;
        }
    }
    let lits = &data[lit_start..];
    out.push((lits.len().min(15) as u8) << 4);
    if lits.len() >= 15 {
        put_len(&mut out, lits.len() - 15);
    }
    out.extend_from_slice(lits);
    out
}

/// Blob `i` of `chunk_size` bytes: its content, codec and compressed form.
pub fn blob(i: usize, chunk_size: u32) -> (Vec<u8>, Codec, Vec<u8>) {
    let n = chunk_size as usize;
    match i % 4 {
        0 => {
            let d = lcg(i as u64 + 1, n);
            (d.clone(), Codec::Raw, d)
        }
        1 => {
            let mut d = vec![(i % 251) as u8 + 1; n];
            d[..8].copy_from_slice(&(i as u64).to_le_bytes());
            let c = lz4_runs(&d);
            (d, Codec::Lz4, c)
        }
        2 => {
            let mut d = lcg(i as u64 + 1, n / 4);
            d.resize(n, (i % 200) as u8 + 3);
            d[..8].copy_from_slice(&(i as u64).to_le_bytes());
            let c = lz4_runs(&d);
            (d, Codec::Lz4, c)
        }
        _ => {
            let mut d = vec![(i % 199) as u8 + 7; n];
            d[..8].copy_from_slice(&(i as u64).to_le_bytes());
            (d.clone(), Codec::Raw, d)
        }
    }
}

pub fn descriptor(spec: &Spec, family: [u8; 16], uuid: [u8; 16]) -> VolumeDescriptor {
    VolumeDescriptor {
        crypto_mode: spec.mode,
        profile: Profile::Default,
        block_size: 4096,
        chunk_size: spec.chunk_size,
        map_fanout: spec.fanout,
        id_len: spec.id_len,
        codec_policy: CodecPolicy::Auto,
        codec_level: 1,
        volume_uuid: VolumeUuid(uuid),
        family_id: FamilyId(family),
        kmk_fingerprint: [0; 16],
        created_at: TimeMs(CREATED_AT),
        lineage: Vec::new(),
        mac: [0; 32],
    }
}

pub fn store_opts(spec: &Spec) -> StoreOptions {
    StoreOptions {
        allow_tmpfs: true,
        segment_target: spec.segment_target,
        ..StoreOptions::default()
    }
}

/// Builds the volume.
pub fn build(spec: &Spec) -> Built {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = tmp.path().to_path_buf();
    let seed = 0x40 + spec.mode as u8;
    let family = [seed; 16];
    let uuid = [seed ^ 0x55; 16];
    let mut desc = descriptor(spec, family, uuid);
    let dir = base.join(VolumeUuid(uuid).to_string());
    std::fs::create_dir_all(&dir).expect("volume dir");

    let (root, key_file, keyring_bytes) = match spec.mode {
        CryptoMode::Plain => (None, None, None),
        CryptoMode::Integrity => {
            let kf = dir.join("integrity.key");
            let c = keyring::create_integrity_with_key_file(FamilyId(family), &kf).expect("ik");
            (Some(c.root), Some(kf), Some(c.bytes))
        }
        CryptoMode::Encrypted => {
            let kfp = base.join("kmk.key");
            let kf = KeyFile::generate().expect("key file");
            kf.write_new(&kfp).expect("write key file");
            let c = keyring::create(
                Profile::Default,
                FamilyId(family),
                RootKind::Kmk,
                &NewSlot::KeyFile(&kf),
            )
            .expect("kmk");
            (Some(c.root), Some(kfp), Some(c.bytes))
        }
    };
    let desc_bytes = seal_descriptor(&mut desc, root.as_ref()).expect("seal descriptor");
    let keyring_path = keyring_bytes.map(|b| {
        let p = base
            .join("families")
            .join(FamilyId(family).to_string())
            .join("keyring");
        std::fs::create_dir_all(p.parent().expect("parent")).expect("families dir");
        std::fs::write(&p, b).expect("write keyring");
        p
    });
    let crypto = VolumeCrypto::new(&desc, root, DekGen(0)).expect("crypto");
    let meta: Arc<dyn MetaCodec> = crypto.meta().clone();
    let mut json = VolumeJson::new("fsck-test", &base.to_string_lossy());
    json.retain_last = spec.retain_last;
    let vol =
        LocalVolume::create(&dir, &desc_bytes, &json, meta, store_opts(spec)).expect("create");

    let known = fill(spec, &vol, &dir, &desc, &desc_bytes, &crypto);
    drop(vol);
    Built {
        tmp,
        dir,
        spec: spec.clone(),
        known,
        desc,
        desc_bytes,
        crypto,
        key_file,
        keyring_path,
    }
}

fn fill(
    spec: &Spec,
    vol: &LocalVolume,
    dir: &Path,
    desc: &VolumeDescriptor,
    desc_bytes: &[u8],
    crypto: &VolumeCrypto,
) -> Known {
    let extents = crypto.extents().clone();
    let size = spec.chunks * u64::from(spec.chunk_size);
    let levels = levels_for(size, spec.chunk_size, spec.fanout).expect("levels");
    let empty = empty_hashes(spec.fanout, levels, |b| {
        extents.chunk_id(ExtentKind::Node, b).0
    })
    .expect("empty hashes");
    let volume_hash = crypto.meta().object_hash(desc_bytes);
    let commit_codec = crypto.meta();

    // Genesis.
    let genesis = Full::genesis(
        volume_hash,
        size,
        empty[usize::from(levels) - 1],
        TimeMs(CREATED_AT),
    );
    let sealed = commit_codec
        .seal_commit(&Commit::Full(genesis))
        .expect("seal genesis");
    vol.put(CommitSeq(0), &sealed.bytes).expect("put genesis");
    vol.raise_rollback_floor(&lunix_format::local::RollbackFloor {
        max_commit_seq: CommitSeq(0),
        tip_hash: sealed.hash,
    })
    .expect("floor");
    let mut parent_hash = sealed.hash;
    let mut chain_floor = 0u64;

    let idx = LsmIndex::open(dir, vol.ids(), spec.id_len).expect("index");
    let map = RadixMap::new(
        extents.clone(),
        Arc::new(vol.clone()) as Arc<dyn NodeSource>,
        spec.chunks,
        MapConfig::new(spec.fanout, spec.id_len),
    )
    .expect("map");

    let mut known = Known::default();
    let mut stored: BTreeMap<usize, (ChunkId, lunix_format::Hint, u32, u32)> = BTreeMap::new();
    let mut content: BTreeMap<u64, usize> = BTreeMap::new();
    let mut high_seg = None;
    let mut runs_named: BTreeSet<ObjId> = BTreeSet::new();
    let mut tip_hash = parent_hash;
    for k in 1..=spec.commits {
        // The cut's script: add `per_commit` chunks, overwrite some, discard a few.
        let mut ops: Vec<(u64, Option<usize>)> = Vec::new();
        let first = (k - 1) * spec.per_commit;
        for j in 0..spec.per_commit {
            let chunk = first + j;
            if chunk >= spec.chunks {
                break;
            }
            // Repeats (dedup): every 5th chunk copies the blob of chunk - 3.
            let b = if j % 5 == 4 {
                (chunk as usize + spec.blobs - 3) % spec.blobs
            } else {
                chunk as usize % spec.blobs
            };
            ops.push((chunk, Some(b)));
        }
        if k > 1 {
            for j in 0..20 {
                ops.push((j * 7, Some((j as usize * 13 + k as usize) % spec.blobs)));
            }
            for j in 0..10 {
                ops.push((first.saturating_sub(1).saturating_sub(j * 3 % 5), None));
            }
        }
        // Store the blobs that are new.
        let mut new_segs: Vec<ObjId> = Vec::new();
        let mut writer: Option<Box<dyn lunix_iface::store::SegmentWriter>> = None;
        let mut writes = None;
        for (_, b) in &ops {
            let Some(b) = *b else { continue };
            if stored.contains_key(&b) {
                continue;
            }
            let (data, codec, comp) = blob(b, spec.chunk_size);
            let id = extents.chunk_id(ExtentKind::Data, &data);
            let mut ext = Vec::new();
            extents
                .seal(
                    ExtentKind::Data,
                    &id,
                    &EncodingDesc {
                        codec,
                        level: Level(0),
                        ulen: spec.chunk_size,
                        dict_id: None,
                    },
                    &comp,
                    &mut ext,
                )
                .expect("seal extent");
            let w = writer.get_or_insert_with(|| vol.open_segment().expect("open segment"));
            let hint = w.append_extent(&id, None, &ext).expect("append");
            stored.insert(
                b,
                (
                    id,
                    hint.with_seg(w.id()),
                    comp.len() as u32,
                    spec.chunk_size,
                ),
            );
            writes = Some(WriteSeqRange {
                first: WriteSeq(1),
                last: WriteSeq(k * 100),
            });
            if w.is_full() {
                let w = writer.take().expect("writer");
                new_segs.push(w.id());
                w.seal(writes).expect("seal segment");
            }
        }
        if let Some(w) = writer.take() {
            new_segs.push(w.id());
            w.seal(writes).expect("seal segment");
        }
        // Index inserts after the seals.
        for (_, (id, hint, _, _)) in stored
            .iter()
            .filter(|(_, (_, h, _, _))| new_segs.contains(&h.seg))
        {
            idx.insert(*id, *hint).expect("index insert");
        }
        if let Some(m) = new_segs.iter().max() {
            high_seg = Some(high_seg.map_or(*m, |h: ObjId| h.max(*m)));
        }
        let runs = match high_seg {
            Some(h) => idx.persist(h).expect("persist"),
            None => Vec::new(),
        };
        runs_named.extend(runs.iter().map(|r| r.id));
        // The map update.
        let mut txn = map.begin_update().expect("begin");
        for (chunk, b) in &ops {
            match b {
                Some(b) => {
                    let (id, hint, _, _) = &stored[b];
                    txn.set(
                        ChunkNo(*chunk),
                        DataEntry {
                            lineage: LineageIdx(0),
                            seg: hint.seg,
                            off: hint.off,
                            clen: hint.clen,
                            prefix: IdPrefix::of(id, spec.id_len),
                        },
                    )
                    .expect("set");
                    content.insert(*chunk, *b);
                }
                None => {
                    txn.set_hole(ChunkNo(*chunk)).expect("hole");
                    content.remove(chunk);
                }
            }
        }
        txn.commit().expect("txn");
        let mut pack = vol.open_pack().expect("pack");
        let res = map.seal(&mut *pack).expect("map seal");
        let packs: Vec<ObjId> = if res.new_packs.is_empty() {
            Vec::new()
        } else {
            let p = pack.seal().expect("pack seal");
            vec![p.pack]
        };
        known.packs.extend(&packs);
        // The commit.
        chain_floor = chain_floor.max(k.saturating_sub(u64::from(spec.retain_last) - 1));
        let full = Full {
            common: CommonHeader {
                kind: CommitKind::Full,
                commit_seq: CommitSeq(k),
                epoch: Epoch(0),
                parent_hash,
                created_at: TimeMs(if spec.clock_back && k == spec.commits {
                    CREATED_AT + (k - 1) * 1000 - 500
                } else {
                    CREATED_AT + k * 1000
                }),
            },
            cut_seq: WriteSeq(k * 100),
            size,
            chain_floor: CommitSeq(chain_floor),
            map_root: res.root,
            heat: None,
            map_packs: packs,
            new_segments: new_segs.clone(),
            index_packs: runs.iter().map(|r| r.id).collect(),
        };
        let sealed = commit_codec
            .seal_commit(&Commit::Full(full))
            .expect("seal commit");
        vol.put(CommitSeq(k), &sealed.bytes).expect("put commit");
        vol.raise_rollback_floor(&lunix_format::local::RollbackFloor {
            max_commit_seq: CommitSeq(k),
            tip_hash: sealed.hash,
        })
        .expect("floor");
        parent_hash = sealed.hash;
        tip_hash = sealed.hash;
        known.segments.extend(&new_segs);
        known.tip = k;
        known.cut_seq = k * 100;
    }
    known.tip_hash = tip_hash;
    known.runs = runs_named.into_iter().collect();
    // Known values from the final content.
    known.mapped = content.len() as u64;
    known.holes = spec.chunks - known.mapped;
    let distinct: BTreeSet<usize> = content.values().copied().collect();
    known.unique = distinct.len() as u64;
    for b in &distinct {
        let (_, _, clen, ulen) = stored[b];
        known.unique_clen += u64::from(clen);
        known.unique_ulen += u64::from(ulen);
    }
    known.map = content;
    drop(idx);
    // The checkpoint (a hint) and the journal.
    vol.store(&CheckpointSlot {
        slot_seq: 1,
        volume_uuid: desc.volume_uuid,
        last_commit_seq: known.tip,
        id_high: vol.ids().high_water(),
        lane_tail_ordinals: vec![1; usize::from(spec.lanes)],
        stats: vec![1, 2, 3],
    })
    .expect("checkpoint");
    vol.store_condemned(&lunix_format::gcfile::Condemned {
        body_ver: 1,
        body: vec![0, 1, 2, 3, 4, 5, 6, 7],
    })
    .expect("condemned");
    write_journal(spec, dir, desc.volume_uuid);
    known
}

/// Writes ring files holding exactly the records `seqs` (lane = seq % lanes).
pub fn write_journal_seqs(dir: &Path, uuid: VolumeUuid, lanes: u16, seqs: &[u64]) {
    let gen = 0x1234_5678_9abc_def0u64;
    for lane in 0..lanes {
        let mut file = Vec::new();
        FileHeader {
            lane,
            ordinal: 1,
            volume_uuid: uuid,
            generation: gen,
        }
        .encode(&mut file)
        .expect("header");
        let mine: Vec<u64> = seqs
            .iter()
            .copied()
            .filter(|s| s % u64::from(lanes) == u64::from(lane))
            .collect();
        for batch in mine.chunks(10) {
            let mut b = BatchBuilder::new(gen, file.len() as u64).expect("batch");
            for s in batch {
                b.push_write(WriteSeq(*s), 0, *s % 64, &lcg(*s, 4096))
                    .expect("push");
            }
            file.extend_from_slice(&b.finish());
        }
        file.resize(1 << 20, 0);
        std::fs::write(
            dir.join("journal").join(format!("q{lane:02x}-00000001.j")),
            file,
        )
        .expect("write journal");
    }
}

/// Lane `l` gets the records whose `write_seq % lanes == l`, in batches of 10.
pub fn write_journal(spec: &Spec, dir: &Path, uuid: VolumeUuid) {
    let gen = 0x1234_5678_9abc_def0u64;
    let file_len: usize = 1 << 20;
    for lane in 0..spec.lanes {
        let mut file = Vec::new();
        FileHeader {
            lane,
            ordinal: 1,
            volume_uuid: uuid,
            generation: gen,
        }
        .encode(&mut file)
        .expect("header");
        let seqs: Vec<u64> = (1..=spec.journal_records)
            .filter(|s| s % u64::from(spec.lanes) == u64::from(lane))
            .collect();
        for batch in seqs.chunks(10) {
            let mut b = BatchBuilder::new(gen, file.len() as u64).expect("batch");
            for s in batch {
                let data = lcg(*s, 4096);
                b.push_write(WriteSeq(*s), 0, *s % 64, &data).expect("push");
            }
            file.extend_from_slice(&b.finish());
        }
        assert!(
            file.len() <= file_len,
            "journal file too small for the records"
        );
        file.resize(file_len, 0);
        std::fs::write(
            dir.join("journal").join(format!("q{lane:02x}-00000001.j")),
            file,
        )
        .expect("write journal");
    }
}

// ---------------------------------------------------------------------------
// Running and tampering.

impl Built {
    /// Options with the credential of this volume.
    pub fn opts(&self) -> Options {
        let mut o = Options::default();
        o.credentials.key_file = self.key_file.clone();
        o
    }

    pub fn deep(&self) -> Options {
        Options {
            deep: true,
            ..self.opts()
        }
    }

    pub fn fsck(&self, opts: &Options) -> Report {
        check(&self.dir, opts)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    pub fn read(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.path(rel)).expect("read")
    }

    pub fn write(&self, rel: &str, bytes: &[u8]) {
        std::fs::write(self.path(rel), bytes).expect("write");
    }

    /// Flips one byte (xor 0x5a) at `at` of `rel`.
    pub fn flip(&self, rel: &str, at: usize) {
        let mut b = self.read(rel);
        b[at] ^= 0x5a;
        self.write(rel, &b);
    }

    pub fn truncate(&self, rel: &str, len: usize) {
        let mut b = self.read(rel);
        b.truncate(len);
        self.write(rel, &b);
    }

    pub fn seg_rel(&self, id: ObjId) -> String {
        format!("segments/{:016x}.seg", id.raw())
    }

    pub fn pack_rel(&self, id: ObjId) -> String {
        format!("map/{:016x}", id.raw())
    }

    pub fn commit_rel(&self, seq: u64) -> String {
        format!("state/commits/{seq:016x}")
    }

    /// Reopens the store (takes the lock; drop it before running fsck).
    pub fn open_store(&self) -> LocalVolume {
        let meta: Arc<dyn MetaCodec> = self.crypto.meta().clone();
        LocalVolume::open(&self.dir, meta, store_opts(&self.spec)).expect("open store")
    }

    pub fn ovh(&self) -> usize {
        self.desc.crypto_mode.extent_ovh() as usize
    }
}

/// Every `*.seg` name of `dir`, ascending.
pub fn list(dir: &Path, sub: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir.join(sub))
        .expect("read_dir")
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    v.sort();
    v
}

pub const ALL_MODES: [CryptoMode; 3] = [
    CryptoMode::Encrypted,
    CryptoMode::Integrity,
    CryptoMode::Plain,
];

pub fn mode_name(m: CryptoMode) -> &'static str {
    match m {
        CryptoMode::Plain => "plain",
        CryptoMode::Integrity => "integrity",
        CryptoMode::Encrypted => "encrypted",
    }
}

/// Stands in for the proposed `VolumeDir::reset_rollback_floor`: rewrites
/// `state/pins` with the format types.
pub struct DirectPins;

impl lunix_fsck::FloorLowering for DirectPins {
    fn set_floor(
        &self,
        vol: &dyn VolumeDir,
        new: &lunix_format::local::RollbackFloor,
        _reason: &'static str,
    ) -> lunix_iface::Result<()> {
        let path = vol.dir().join("state/pins");
        let mut f = lunix_format::local::LocalPinsFile::decode(&std::fs::read(&path)?)?;
        f.floor = Some(*new);
        let mut out = Vec::new();
        f.encode(&mut out)?;
        std::fs::write(&path, out)?;
        Ok(())
    }
}

/// The records of every ring file of `dir`: `(write_seq, offset in file)`.
pub fn journal_records(dir: &Path, lane: u16) -> Vec<(u64, usize)> {
    let bytes = std::fs::read(dir.join("journal").join(format!("q{lane:02x}-00000001.j")))
        .expect("journal file");
    let res = lunix_format::journal::scan_file(&bytes, WriteSeq(0)).expect("scan");
    res.records
        .iter()
        .map(|r| (r.header.write_seq.0, r.header.off as usize))
        .collect()
}

pub trait HintExt {
    fn with_seg(self, seg: ObjId) -> lunix_format::Hint;
}

impl HintExt for lunix_format::Hint {
    fn with_seg(self, seg: ObjId) -> lunix_format::Hint {
        lunix_format::Hint { seg, ..self }
    }
}
