use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lunix_format::extent::{Cch1Header, Extent};
use lunix_format::gcfile::Condemned;
use lunix_format::local::{CheckpointSlot, DeviceRecord, LocalPins, VolumeJson};
use lunix_format::mode::OVH_UNENCRYPTED;
use lunix_format::node::NodeLoc;
use lunix_format::segment::{ContainerKind, FooterBody, FooterEntry};
use lunix_format::{
    ChunkId, Codec, CommitSeq, Hint, Level, ObjId, PackId, SegId, VolumeUuid, WriteSeq,
};
use lunix_iface::contract::store::StoreFixture;
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::store::*;
use lunix_iface::{Error, Result};

use super::lock;

const OVH: usize = OVH_UNENCRYPTED as usize;

pub struct Ids(AtomicU64);

impl IdAllocator for Ids {
    fn allocate(&self) -> Result<ObjId> {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ObjId::new(0, n)?)
    }
    fn high_water(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn observe(&self, raw: u64) {
        self.0.fetch_max(raw, Ordering::SeqCst);
    }
}

struct SegFile {
    bytes: Vec<u8>,
    footer: FooterBody,
}

#[derive(Default)]
struct State {
    segments: BTreeMap<u64, SegFile>,
    packs: BTreeMap<u64, Vec<u8>>,
    commits: BTreeMap<u64, Vec<u8>>,
    aside: BTreeMap<u64, Vec<u8>>,
    checkpoint: Vec<CheckpointSlot>,
    condemned: Option<Condemned>,
    evidence: BTreeMap<u64, Vec<u8>>,
}

#[derive(Clone)]
pub struct TStore {
    ids: Arc<Ids>,
    st: Arc<Mutex<State>>,
}

impl TStore {
    pub fn new() -> Self {
        Self {
            ids: Arc::new(Ids(AtomicU64::new(0))),
            st: Arc::new(Mutex::new(State::default())),
        }
    }
}

const SEG_FOOTER_BYTES: u64 = 64;

struct SegW {
    st: Arc<Mutex<State>>,
    id: SegId,
    buf: Vec<u8>,
    entries: Vec<FooterEntry>,
}

impl SegmentWriter for SegW {
    fn id(&self) -> SegId {
        self.id
    }
    fn append_extent(
        &mut self,
        chunk_id: &ChunkId,
        _dict_id: Option<u32>,
        extent: &[u8],
    ) -> Result<Hint> {
        let header = Cch1Header::parse(extent)?;
        let clen = u32::try_from(
            extent
                .len()
                .checked_sub(OVH)
                .ok_or(Error::invalid("not an extent"))?,
        )
        .map_err(|_| Error::invalid("extent too long"))?;
        let off = u32::try_from(self.buf.len()).map_err(|_| Error::NoSpace)?;
        self.buf.extend_from_slice(extent);
        self.entries.push(FooterEntry {
            chunk_id: *chunk_id,
            off,
            clen,
            ulen: header.ulen as u32,
            codec: header.codec,
        });
        Ok(Hint {
            seg: self.id,
            off,
            clen,
        })
    }
    fn len(&self) -> u64 {
        self.buf.len() as u64
    }
    fn is_full(&self) -> bool {
        self.buf.len() >= 16 << 20
    }
    fn seal(self: Box<Self>, writes: Option<WriteSeqRange>) -> Result<SealedSegment> {
        let (lo, hi) = writes.map_or((WriteSeq(0), WriteSeq(0)), |r| (r.first, r.last));
        let extents = self.entries.len() as u32;
        let size = self.buf.len() as u64 + SEG_FOOTER_BYTES;
        let footer = FooterBody {
            container_kind: ContainerKind::Segment,
            object_id: self.id,
            extents_len: self.buf.len() as u64,
            write_seq_lo: lo,
            write_seq_hi: hi,
            seg_hash: [0; 32],
            entries: self.entries,
            dict_ids: Vec::new(),
        };
        lock(&self.st).segments.insert(
            self.id.raw(),
            SegFile {
                bytes: self.buf,
                footer,
            },
        );
        Ok(SealedSegment {
            seg: self.id,
            size_bytes: size,
            extents,
        })
    }
}

impl SegmentStore for TStore {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        Ok(Box::new(SegW {
            st: self.st.clone(),
            id: self.ids.allocate()?,
            buf: Vec::new(),
            entries: Vec::new(),
        }))
    }
    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        let st = lock(&self.st);
        let f = st
            .segments
            .get(&hint.seg.raw())
            .ok_or(Error::not_found("segment"))?;
        let (a, b) = (
            hint.off as usize,
            hint.off as usize + hint.clen as usize + OVH,
        );
        out.extend_from_slice(f.bytes.get(a..b).ok_or(Error::Corrupt {
            what: "segment",
            detail: "hint outside file",
        })?);
        Ok(())
    }
    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        if !lock(&self.st).segments.contains_key(&hint.seg.raw()) {
            return Err(Error::not_found("segment"));
        }
        let fd = OwnedFd::from(std::fs::File::open("/dev/null")?);
        Ok(ExtentLocation {
            fd: Arc::new(fd),
            offset: u64::from(hint.off),
            len: hint.clen + OVH as u32,
        })
    }
    fn footer(&self, seg: SegId) -> Result<FooterBody> {
        lock(&self.st)
            .segments
            .get(&seg.raw())
            .map(|f| f.footer.clone())
            .ok_or(Error::not_found("segment"))
    }
    fn list(&self) -> Result<Vec<SegmentInfo>> {
        Ok(lock(&self.st)
            .segments
            .iter()
            .map(|(&k, f)| SegmentInfo {
                seg: ObjId::from_raw(k).expect("id"),
                size_bytes: f.bytes.len() as u64 + SEG_FOOTER_BYTES,
            })
            .collect())
    }
    fn delete(&self, seg: SegId) -> Result<u64> {
        Ok(lock(&self.st)
            .segments
            .remove(&seg.raw())
            .map_or(0, |f| f.bytes.len() as u64 + SEG_FOOTER_BYTES))
    }
}

struct PackW {
    st: Arc<Mutex<State>>,
    id: PackId,
    buf: Vec<u8>,
    nodes: u32,
}

impl PackWrite for PackW {
    fn id(&self) -> PackId {
        self.id
    }
    fn append_node(&mut self, _hash: &[u8; 32], extent: &[u8]) -> Result<NodeLoc> {
        let off = u32::try_from(self.buf.len()).map_err(|_| Error::NoSpace)?;
        let len = u32::try_from(extent.len()).map_err(|_| Error::invalid("node too long"))?;
        self.buf.extend_from_slice(extent);
        self.nodes += 1;
        Ok(NodeLoc {
            pack: self.id,
            off,
            len,
        })
    }
    fn len(&self) -> u64 {
        self.buf.len() as u64
    }
    fn seal(self: Box<Self>) -> Result<SealedPack> {
        let size = self.buf.len() as u64;
        lock(&self.st).packs.insert(self.id.raw(), self.buf);
        Ok(SealedPack {
            pack: self.id,
            size_bytes: size,
            nodes: self.nodes,
        })
    }
}

impl PackRead for TStore {
    fn read_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        let st = lock(&self.st);
        let p = st
            .packs
            .get(&loc.pack.raw())
            .ok_or(Error::not_found("pack"))?;
        let (a, b) = (loc.off as usize, loc.off as usize + loc.len as usize);
        out.extend_from_slice(p.get(a..b).ok_or(Error::Corrupt {
            what: "pack",
            detail: "range outside pack",
        })?);
        Ok(())
    }
    fn pack_footer(&self, _pack: PackId) -> Result<FooterBody> {
        Err(Error::Unsupported {
            what: "trivial pack footer",
        })
    }
    fn list_packs(&self) -> Result<Vec<PackInfo>> {
        Ok(lock(&self.st)
            .packs
            .iter()
            .map(|(&k, b)| PackInfo {
                pack: ObjId::from_raw(k).expect("id"),
                size_bytes: b.len() as u64,
            })
            .collect())
    }
}

impl PackStore for TStore {
    fn open_pack(&self) -> Result<Box<dyn PackWrite>> {
        Ok(Box::new(PackW {
            st: self.st.clone(),
            id: self.ids.allocate()?,
            buf: Vec::new(),
            nodes: 0,
        }))
    }
    fn delete_pack(&self, pack: PackId) -> Result<u64> {
        Ok(lock(&self.st)
            .packs
            .remove(&pack.raw())
            .map_or(0, |b| b.len() as u64))
    }
}

impl CommitStore for TStore {
    fn put(&self, seq: CommitSeq, object: &[u8]) -> Result<()> {
        let mut st = lock(&self.st);
        if st.commits.contains_key(&seq.0) || st.aside.contains_key(&seq.0) {
            return Err(Error::AlreadyExists { what: "commit" });
        }
        st.commits.insert(seq.0, object.to_vec());
        Ok(())
    }
    fn get(&self, seq: CommitSeq) -> Result<Vec<u8>> {
        lock(&self.st)
            .commits
            .get(&seq.0)
            .cloned()
            .ok_or(Error::not_found("commit"))
    }
    fn list(&self) -> Result<Vec<CommitInfo>> {
        Ok(lock(&self.st)
            .commits
            .iter()
            .map(|(&k, v)| CommitInfo {
                seq: CommitSeq(k),
                size_bytes: v.len() as u64,
            })
            .collect())
    }
    fn tip(&self) -> Result<Option<CommitSeq>> {
        Ok(lock(&self.st)
            .commits
            .keys()
            .next_back()
            .map(|&k| CommitSeq(k)))
    }
    fn delete(&self, seq: CommitSeq, chain_floor: CommitSeq) -> Result<u64> {
        let mut st = lock(&self.st);
        let tip = st.commits.keys().next_back().copied();
        if seq >= chain_floor || Some(seq.0) == tip {
            return Err(Error::Refused {
                why: "commit is the tip or at or above chain_floor",
            });
        }
        Ok(st.commits.remove(&seq.0).map_or(0, |v| v.len() as u64))
    }
    fn set_aside(&self, seq: CommitSeq) -> Result<()> {
        let mut st = lock(&self.st);
        match st.commits.keys().next_back().copied() {
            None => Err(Error::not_found("commit")),
            Some(t) if t != seq.0 => Err(Error::Refused {
                why: "only the tip can be set aside",
            }),
            Some(t) => {
                let v = st.commits.remove(&t).expect("tip exists");
                st.aside.insert(t, v);
                Ok(())
            }
        }
    }
}

impl CheckpointStore for TStore {
    fn load(&self) -> Result<Option<CheckpointSlot>> {
        Ok(lock(&self.st)
            .checkpoint
            .iter()
            .max_by_key(|s| s.slot_seq)
            .cloned())
    }
    fn store(&self, slot: &CheckpointSlot) -> Result<()> {
        let mut st = lock(&self.st);
        let mut slot = slot.clone();
        slot.slot_seq = st.checkpoint.iter().map(|s| s.slot_seq).max().unwrap_or(0) + 1;
        st.checkpoint.push(slot);
        st.checkpoint.sort_by_key(|s| s.slot_seq);
        while st.checkpoint.len() > 2 {
            st.checkpoint.remove(0);
        }
        Ok(())
    }
    fn zero(&self) -> Result<()> {
        lock(&self.st).checkpoint.clear();
        Ok(())
    }
}

impl GcStateStore for TStore {
    fn load_condemned(&self) -> Result<Option<Condemned>> {
        Ok(lock(&self.st).condemned.clone())
    }
    fn store_condemned(&self, list: &Condemned) -> Result<()> {
        lock(&self.st).condemned = Some(list.clone());
        Ok(())
    }
    fn put_evidence(&self, pass: u64, lines: &[u8]) -> Result<()> {
        let mut st = lock(&self.st);
        if st.evidence.contains_key(&pass) {
            return Err(Error::AlreadyExists {
                what: "gc evidence",
            });
        }
        st.evidence.insert(pass, lines.to_vec());
        Ok(())
    }
    fn evidence_passes(&self) -> Result<Vec<u64>> {
        Ok(lock(&self.st).evidence.keys().copied().collect())
    }
}

impl PinSource for TStore {
    fn roots(&self) -> Result<Vec<PinRoot>> {
        Ok(Vec::new())
    }
}

impl VolumeDir for TStore {
    fn dir(&self) -> &std::path::Path {
        std::path::Path::new("/t")
    }
    fn uuid(&self) -> VolumeUuid {
        VolumeUuid([1; 16])
    }
    fn volume_bytes(&self) -> Result<Vec<u8>> {
        Ok(b"volume".to_vec())
    }
    fn volume_json(&self) -> Result<VolumeJson> {
        Ok(VolumeJson::new("t", "/t"))
    }
    fn store_volume_json(&self, _json: &VolumeJson) -> Result<()> {
        Ok(())
    }
    fn device_record(&self) -> Result<Option<DeviceRecord>> {
        Ok(None)
    }
    fn store_device_record(&self, _rec: &DeviceRecord) -> Result<()> {
        Ok(())
    }
    fn clear_device_record(&self) -> Result<()> {
        Ok(())
    }
    fn local_pins(&self) -> Result<Option<LocalPins>> {
        Ok(None)
    }
    fn store_local_pins(&self, _pins: &LocalPins) -> Result<()> {
        Ok(())
    }
    fn ids(&self) -> Arc<dyn IdAllocator> {
        self.ids.clone()
    }
    fn leftovers(&self) -> Result<Vec<Leftover>> {
        Ok(Vec::new())
    }
    fn remove_leftovers(&self) -> Result<u32> {
        Ok(0)
    }
    fn sweep_orphans(&self, req: &SweepRequest<'_>) -> Result<SweepReport> {
        let mut st = lock(&self.st);
        let mut report = SweepReport {
            highest_id_before: st
                .segments
                .keys()
                .chain(st.packs.keys())
                .copied()
                .max()
                .unwrap_or(0),
            ..SweepReport::default()
        };
        let seg_ids: Vec<u64> = st.segments.keys().copied().collect();
        for k in seg_ids {
            let id = ObjId::from_raw(k).expect("id");
            if req.named_segments.contains(&id) {
                continue;
            }
            if k > req.high_water_mark {
                st.segments.remove(&k);
                report.deleted_segments.push(id);
            } else {
                report.kept_unreferenced.push(id);
            }
        }
        let pack_ids: Vec<u64> = st.packs.keys().copied().collect();
        for k in pack_ids {
            let id = ObjId::from_raw(k).expect("id");
            if req.named_packs.contains(&id) {
                continue;
            }
            if k > req.high_water_mark {
                st.packs.remove(&k);
                report.deleted_packs.push(id);
            } else {
                report.kept_unreferenced.push(id);
            }
        }
        Ok(report)
    }
}

/// Newtype so the fixture can hand out `&dyn` of the `Arc`.
pub struct SFix {
    pub store: TStore,
}

impl SFix {
    pub fn new() -> Self {
        Self {
            store: TStore::new(),
        }
    }
}

pub fn extent_bytes(id: &ChunkId, payload_len: usize) -> Vec<u8> {
    let payload = vec![0xa5u8; payload_len];
    let e = Extent {
        header: Cch1Header {
            codec: Codec::Raw,
            level: Level(0),
            ulen: payload_len as u64,
        },
        chunk_id: *id,
        crypto: None,
        payload: &payload,
        tag: [0; 16],
    };
    let mut out = Vec::new();
    e.encode(&mut out).expect("encode extent");
    out
}

impl StoreFixture for SFix {
    fn segments(&self) -> &dyn SegmentStore {
        &self.store
    }
    fn packs(&self) -> &dyn PackStore {
        &self.store
    }
    fn commits(&self) -> &dyn CommitStore {
        &self.store
    }
    fn checkpoint(&self) -> &dyn CheckpointStore {
        &self.store
    }
    fn gc_state(&self) -> &dyn GcStateStore {
        &self.store
    }
    fn volume(&self) -> &dyn VolumeDir {
        &self.store
    }
    fn data_extent(&self, id: &ChunkId, payload_len: usize) -> Vec<u8> {
        extent_bytes(id, payload_len)
    }
    fn node_extent(&self, hash: &[u8; 32], payload_len: usize) -> Vec<u8> {
        extent_bytes(&ChunkId(*hash), payload_len)
    }
    fn reopen(&mut self) {}
}
