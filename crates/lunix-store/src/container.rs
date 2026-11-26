//! Segments and map packs: one container layout (`[extent]* footer trailer`),
//! written through [`ContainerWriter`] and read through [`Container`].

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;

use lunix_format::node::NodeLoc;
use lunix_format::paths;
use lunix_format::segment::{
    decode_footer_frame, footer_body_cap, footer_frame, ContainerKind, FooterBody, FooterEntry,
    FrameCompression, Trailer, MAX_CONTAINER_LEN, MAX_DATA_ULEN, MAX_NODE_ULEN, TRAILER_LEN,
};
use lunix_format::{ChunkId, Codec, Hint, ObjId, ObjectKind, WriteSeq};
use lunix_iface::journal::WriteSeqRange;
use lunix_iface::store::{
    ExtentLocation, IdAllocator, PackInfo, PackRead, PackStore, PackWrite, SealedPack,
    SealedSegment, SegmentInfo, SegmentStore, SegmentWriter,
};
use lunix_iface::{Error, Result};

use rustix::fs::fstat;

use crate::fsops::{io_err, io_or_missing, list_hex_ids, pread_exact, PACK_POINTS, SEGMENT_POINTS};
use crate::volume::{Inner, LocalVolume};

/// The extent region of a container may not grow past this (every `Hint.off`
/// is a `u32`; leave room for the footer and trailer).
const MAX_EXTENT_REGION: u64 = MAX_CONTAINER_LEN / 2;

/// Writes are buffered up to this many bytes before they reach the file.
const WRITE_BUFFER: usize = 1 << 20;

/// A sealed container open for reading: the descriptor and the validated
/// trailer.
#[derive(Debug)]
pub(crate) struct Container {
    pub(crate) fd: Arc<OwnedFd>,
    pub(crate) id: ObjId,
    pub(crate) kind: ContainerKind,
    pub(crate) trailer: Trailer,
}

fn corrupt(what: &'static str, detail: &'static str) -> Error {
    Error::Corrupt { what, detail }
}

impl Inner {
    fn kind_what(kind: ContainerKind) -> &'static str {
        match kind {
            ContainerKind::Segment => "segment",
            ContainerKind::MapPack => "map pack",
        }
    }

    fn footer_name(kind: ContainerKind, id: ObjId) -> String {
        match kind {
            ContainerKind::Segment => paths::segment(id),
            ContainerKind::MapPack => paths::map_pack(id),
        }
    }

    fn ulen_limit(kind: ContainerKind) -> u32 {
        match kind {
            ContainerKind::Segment => MAX_DATA_ULEN,
            ContainerKind::MapPack => MAX_NODE_ULEN,
        }
    }

    /// Deletes a sealed container (unlink, directory fsync) and returns the
    /// bytes freed; a missing one frees 0. A segment deleted through
    /// `SegmentStore::delete` (GC, `--delete-orphans`) has its own crash
    /// points (`store.segment_delete.*`); the open's sweep of what lies above
    /// the mark uses the common ones (`store.delete.*`), so a kill can tell
    /// the two apart.
    pub(crate) fn delete_container(&self, kind: ContainerKind, id: ObjId) -> Result<u64> {
        self.delete_container_as(kind, id, kind == ContainerKind::Segment)
    }

    /// The open's sweep: as [`delete_container`](Self::delete_container) with
    /// the common crash points for every kind.
    pub(crate) fn sweep_container(&self, kind: ContainerKind, id: ObjId) -> Result<u64> {
        self.delete_container_as(kind, id, false)
    }

    fn delete_container_as(
        &self,
        kind: ContainerKind,
        id: ObjId,
        segment_points: bool,
    ) -> Result<u64> {
        let path = self.container_path(kind, id);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let removed = if segment_points {
            crate::fsops::unlink_segment_durable(self, &path)?
        } else {
            crate::fsops::unlink_durable(self, &path)?
        };
        self.cache().remove(id);
        Ok(if removed { size } else { 0 })
    }

    /// Opens a sealed container and validates its trailer against the file
    /// size and the id in its name. Cached.
    pub(crate) fn open_container(&self, kind: ContainerKind, id: ObjId) -> Result<Arc<Container>> {
        if let Some(c) = self.cache().get(id) {
            if c.kind != kind {
                return Err(corrupt(Self::kind_what(kind), "id belongs to another kind"));
            }
            return Ok(c);
        }
        let what = Self::kind_what(kind);
        let file = File::open(self.container_path(kind, id)).map_err(|e| io_or_missing(e, what))?;
        let size = file.metadata().map_err(io_err)?.len();
        let fd = Arc::new(OwnedFd::from(file));
        let too_short = corrupt(what, "shorter than a trailer");
        let tail_off = size.checked_sub(TRAILER_LEN as u64).ok_or(too_short)?;
        let mut tail = [0u8; TRAILER_LEN];
        pread_exact(&fd, &mut tail, tail_off).map_err(io_err)?;
        let trailer = Trailer::decode(&tail)?;
        let total = trailer
            .footer_off
            .checked_add(u64::from(trailer.footer_len))
            .and_then(|v| v.checked_add(TRAILER_LEN as u64));
        if total != Some(size) {
            return Err(corrupt(what, "trailer does not match the file size"));
        }
        if trailer.object_id != id || trailer.container_kind != kind {
            return Err(corrupt(what, "trailer names another object"));
        }
        let c = Arc::new(Container {
            fd,
            id,
            kind,
            trailer,
        });
        #[cfg(test)]
        crate::volume::run_open_hook(self);
        // A delete that ran after the `open` above has unlinked the file and
        // found nothing to remove from the cache yet; inserting now would pin
        // the deleted bytes (and their blocks) until eviction. The delete
        // removes from the cache after its unlink, and the link count is read
        // under the cache lock: a descriptor seen linked here is inserted
        // before that removal, so it is removed with it.
        let mut cache = self.cache();
        let linked = fstat(&*c.fd).map_err(|e| io_err(e.into()))?.st_nlink > 0;
        if linked {
            cache.insert(c.clone(), self.max_open_files);
        }
        drop(cache);
        Ok(c)
    }

    /// Bounds-checks an extent range inside the extent region and returns
    /// `(offset, whole length)`.
    fn extent_range(&self, c: &Container, off: u32, clen: u32) -> Result<(u64, u32)> {
        let what = Self::kind_what(c.kind);
        if clen == 0 || clen > Self::ulen_limit(c.kind) {
            return Err(corrupt(what, "extent length out of range"));
        }
        let len = self
            .ovh
            .checked_add(clen)
            .ok_or_else(|| corrupt(what, "extent length overflow"))?;
        let end = u64::from(off) + u64::from(len);
        if end > c.trailer.footer_off {
            return Err(corrupt(what, "extent outside the extent region"));
        }
        Ok((u64::from(off), len))
    }

    /// Appends the stored bytes of one extent to `out`.
    fn read_range(&self, c: &Container, off: u32, clen: u32, out: &mut Vec<u8>) -> Result<()> {
        let (at, len) = self.extent_range(c, off, clen)?;
        let start = out.len();
        out.resize(start + len as usize, 0);
        let res = pread_exact(&c.fd, &mut out[start..], at);
        if let Err(e) = res {
            out.truncate(start);
            return Err(io_err(e));
        }
        // The stored `clen` sits at byte 48 of every extent; a hint that lands
        // in the middle of another extent almost never agrees with it.
        let stored = u32::from_le_bytes([
            out[start + 48],
            out[start + 49],
            out[start + 50],
            out[start + 51],
        ]);
        if stored != clen {
            out.truncate(start);
            return Err(corrupt(
                Self::kind_what(c.kind),
                "hint does not point at an extent",
            ));
        }
        Ok(())
    }

    /// The authenticated, cross-checked footer of a sealed container.
    pub(crate) fn container_footer(&self, c: &Container) -> Result<FooterBody> {
        let what = Self::kind_what(c.kind);
        // The envelope holds a zstd frame of a body of at most `cap` bytes.
        let cap = footer_body_cap(c.trailer.footer_off)? as u64;
        if u64::from(c.trailer.footer_len) > cap + cap / 128 + 1024 {
            return Err(corrupt(what, "footer longer than the limit"));
        }
        let mut env = vec![0u8; c.trailer.footer_len as usize];
        pread_exact(&c.fd, &mut env, c.trailer.footer_off).map_err(io_err)?;
        let name = Self::footer_name(c.kind, c.id);
        let frame = self.meta.open(ObjectKind::Footer, &name, &env)?;
        let body = decode_footer_frame(&frame, c.trailer.footer_off)?;
        body.check_trailer(&c.trailer)?;
        body.check_tiling(self.ovh)?;
        Ok(body)
    }

    /// Streams the extent region through BLAKE3 and compares it with the
    /// authenticated footer's `seg_hash`.
    pub(crate) fn verify_container(&self, kind: ContainerKind, id: ObjId) -> Result<()> {
        let c = self.open_container(kind, id)?;
        let body = self.container_footer(&c)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut at = 0u64;
        while at < c.trailer.footer_off {
            let n = usize::try_from((c.trailer.footer_off - at).min(buf.len() as u64))
                .map_err(|_| corrupt(Self::kind_what(kind), "region length"))?;
            pread_exact(&c.fd, &mut buf[..n], at).map_err(io_err)?;
            hasher.update(&buf[..n]);
            at += n as u64;
        }
        if *hasher.finalize().as_bytes() != body.seg_hash {
            return Err(corrupt(
                Self::kind_what(kind),
                "extent region does not match the footer hash",
            ));
        }
        Ok(())
    }
}

/// The parts of an extent the store reads without a key.
struct Prefix {
    codec: Codec,
    ulen: u32,
    clen: u32,
}

/// Checks that `extent` has the shape of an extent for this volume mode and
/// container kind and returns the fields the footer needs. The tag is the
/// codec's to verify, not the store's.
fn parse_extent(extent: &[u8], ovh: u32, id: &ChunkId, kind: ContainerKind) -> Result<Prefix> {
    let bad = |why| Error::Invalid { why };
    let ovh = ovh as usize;
    if extent.len() <= ovh || extent.len() > ovh + MAX_DATA_ULEN as usize {
        return Err(bad("bytes are not an extent: length"));
    }
    if &extent[0..4] != b"CCH1" || extent[4] != 1 {
        return Err(bad("bytes are not an extent: header"));
    }
    let codec = Codec::from_u16(u16::from_le_bytes([extent[5], extent[6]]))
        .map_err(|_| bad("bytes are not an extent: codec"))?;
    let ulen64 = u64::from_le_bytes(extent[8..16].try_into().map_err(|_| bad("header"))?);
    let ulen = u32::try_from(ulen64).map_err(|_| bad("bytes are not an extent: ulen"))?;
    if ulen == 0 || ulen > Inner::ulen_limit(kind) {
        return Err(bad("bytes are not an extent: ulen"));
    }
    if extent[16..48] != id.0 {
        return Err(bad("extent id differs from the one given"));
    }
    let clen = u32::from_le_bytes(extent[48..52].try_into().map_err(|_| bad("header"))?);
    if clen as usize != extent.len() - ovh {
        return Err(bad("bytes are not an extent: clen"));
    }
    let clen_ok = if codec == Codec::Raw {
        clen == ulen
    } else {
        (1..=ulen).contains(&clen)
    };
    if !clen_ok {
        return Err(bad("bytes are not an extent: clen"));
    }
    Ok(Prefix { codec, ulen, clen })
}

/// Appends extents to one `.open` file and seals it.
pub(crate) struct ContainerWriter {
    inner: Arc<Inner>,
    kind: ContainerKind,
    id: ObjId,
    file: Option<File>,
    buf: Vec<u8>,
    len: u64,
    hasher: blake3::Hasher,
    entries: Vec<FooterEntry>,
    dict_ids: BTreeSet<u32>,
    poisoned: bool,
    sealed: bool,
}

impl ContainerWriter {
    pub(crate) fn start(inner: Arc<Inner>, kind: ContainerKind) -> Result<Self> {
        let id = inner.ids.allocate()?;
        let path = inner.open_path(kind, id);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(path)
            .map_err(io_err)?;
        crate::volume::lock(&inner.open_writers).insert(id.raw());
        Ok(Self {
            inner,
            kind,
            id,
            file: Some(file),
            buf: Vec::new(),
            len: 0,
            hasher: blake3::Hasher::new(),
            entries: Vec::new(),
            dict_ids: BTreeSet::new(),
            poisoned: false,
            sealed: false,
        })
    }

    fn flush(&mut self) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| Error::invalid("writer already sealed"))?;
        let r = file.write_all(&self.buf).map_err(io_err);
        self.buf.clear();
        if r.is_err() {
            self.poisoned = true;
        }
        r
    }

    /// Appends one extent and returns its offset.
    pub(crate) fn append(
        &mut self,
        chunk_id: &ChunkId,
        dict_id: Option<u32>,
        extent: &[u8],
    ) -> Result<u32> {
        if self.poisoned {
            return Err(Error::invalid("writer failed earlier"));
        }
        let p = parse_extent(extent, self.inner.ovh, chunk_id, self.kind)?;
        match (p.codec == Codec::ZstdDict, dict_id) {
            (true, None) => return Err(Error::invalid("codec 3 needs a dictionary id")),
            (false, Some(_)) => {
                return Err(Error::invalid("dictionary id on a non-codec-3 extent"))
            }
            _ => {}
        }
        if self.kind == ContainerKind::MapPack && dict_id.is_some() {
            return Err(Error::invalid("map packs carry no dictionary ids"));
        }
        let new_len = self
            .len
            .checked_add(extent.len() as u64)
            .filter(|n| *n <= MAX_EXTENT_REGION)
            .ok_or_else(|| Error::invalid("container too large"))?;
        let off = u32::try_from(self.len).map_err(|_| Error::invalid("container too large"))?;
        self.buf.extend_from_slice(extent);
        self.hasher.update(extent);
        self.len = new_len;
        self.entries.push(FooterEntry {
            chunk_id: *chunk_id,
            off,
            clen: p.clen,
            ulen: p.ulen,
            codec: p.codec,
        });
        if let Some(d) = dict_id {
            self.dict_ids.insert(d);
        }
        if self.buf.len() >= WRITE_BUFFER {
            self.flush()?;
        }
        Ok(off)
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn id(&self) -> ObjId {
        self.id
    }

    /// Footer, trailer, `fdatasync`, rename, directory `fsync`.
    pub(crate) fn seal(&mut self, writes: Option<WriteSeqRange>) -> Result<(u64, u32)> {
        if self.poisoned {
            return Err(Error::invalid("writer failed earlier"));
        }
        if self.entries.is_empty() {
            return Err(Error::invalid("empty container"));
        }
        self.flush()?;
        let pts = match self.kind {
            ContainerKind::Segment => &SEGMENT_POINTS,
            ContainerKind::MapPack => &PACK_POINTS,
        };
        let mut entries = std::mem::take(&mut self.entries);
        entries.sort_by_key(|e| (e.chunk_id, e.off));
        let count = u32::try_from(entries.len()).map_err(|_| Error::invalid("too many extents"))?;
        let (lo, hi) = match writes {
            Some(r) => (r.first, r.last),
            None => (WriteSeq(0), WriteSeq(0)),
        };
        let body = FooterBody {
            container_kind: self.kind,
            object_id: self.id,
            extents_len: self.len,
            write_seq_lo: lo,
            write_seq_hi: hi,
            seg_hash: *self.hasher.finalize().as_bytes(),
            entries,
            dict_ids: self.dict_ids.iter().copied().collect(),
        };
        let frame = footer_frame(&body, FrameCompression::Zstd(3))?;
        let name = Inner::footer_name(self.kind, self.id);
        let envelope = self.inner.meta.seal(ObjectKind::Footer, &name, &frame)?;
        let trailer = Trailer {
            footer_off: self.len,
            footer_len: u32::try_from(envelope.len())
                .map_err(|_| Error::invalid("footer too large"))?,
            extent_count: count,
            object_id: self.id,
            container_kind: self.kind,
        };
        let mut tail = envelope;
        trailer.encode(&mut tail)?;
        let total = self.len + tail.len() as u64;
        if total > MAX_CONTAINER_LEN {
            return Err(Error::invalid("container too large"));
        }
        let open = self.inner.open_path(self.kind, self.id);
        let sealed = self.inner.container_path(self.kind, self.id);
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| Error::invalid("writer already sealed"))?;
        file.write_all(&tail).map_err(io_err)?;
        (pts.before_fdatasync)();
        self.inner.fs.fdatasync(file).map_err(io_err)?;
        (pts.after_fdatasync)();
        self.file = None;
        self.inner.fs.rename(&open, &sealed, true).map_err(io_err)?;
        // From here the sealed name exists; Drop must not delete it.
        self.sealed = true;
        (pts.after_rename)();
        self.inner
            .fs
            .fsync_dir(sealed.parent().unwrap_or(&self.inner.dir))
            .map_err(io_err)?;
        (pts.after_dirsync)();
        Ok((total, count))
    }
}

impl Drop for ContainerWriter {
    fn drop(&mut self) {
        crate::volume::lock(&self.inner.open_writers).remove(&self.id.raw());
        if !self.sealed {
            self.file = None;
            let _ = std::fs::remove_file(self.inner.open_path(self.kind, self.id));
        }
    }
}

struct SegWriter(ContainerWriter);

impl SegmentWriter for SegWriter {
    fn id(&self) -> ObjId {
        self.0.id()
    }

    fn append_extent(
        &mut self,
        chunk_id: &ChunkId,
        dict_id: Option<u32>,
        extent: &[u8],
    ) -> Result<Hint> {
        let off = self.0.append(chunk_id, dict_id, extent)?;
        let clen = u32::try_from(extent.len() - self.0.inner.ovh as usize)
            .map_err(|_| Error::invalid("extent too large"))?;
        Ok(Hint {
            seg: self.0.id(),
            off,
            clen,
        })
    }

    fn len(&self) -> u64 {
        self.0.len()
    }

    fn is_full(&self) -> bool {
        self.0.len() >= self.0.inner.segment_target
    }

    fn seal(mut self: Box<Self>, writes: Option<WriteSeqRange>) -> Result<SealedSegment> {
        let (size, extents) = self.0.seal(writes)?;
        Ok(SealedSegment {
            seg: self.0.id(),
            size_bytes: size,
            extents,
        })
    }
}

struct PackWriter(ContainerWriter);

impl PackWrite for PackWriter {
    fn id(&self) -> ObjId {
        self.0.id()
    }

    fn append_node(&mut self, hash: &[u8; 32], extent: &[u8]) -> Result<NodeLoc> {
        let off = self.0.append(&ChunkId(*hash), None, extent)?;
        let len =
            u32::try_from(extent.len()).map_err(|_| Error::invalid("node extent too large"))?;
        Ok(NodeLoc {
            pack: self.0.id(),
            off,
            len,
        })
    }

    fn len(&self) -> u64 {
        self.0.len()
    }

    fn seal(mut self: Box<Self>) -> Result<SealedPack> {
        let (size, nodes) = self.0.seal(None)?;
        Ok(SealedPack {
            pack: self.0.id(),
            size_bytes: size,
            nodes,
        })
    }
}

impl SegmentStore for LocalVolume {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        let w = ContainerWriter::start(self.inner.clone(), ContainerKind::Segment)?;
        Ok(Box::new(SegWriter(w)))
    }

    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        let c = self
            .inner
            .open_container(ContainerKind::Segment, hint.seg)?;
        self.inner.read_range(&c, hint.off, hint.clen, out)
    }

    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        let c = self
            .inner
            .open_container(ContainerKind::Segment, hint.seg)?;
        let (offset, len) = self.inner.extent_range(&c, hint.off, hint.clen)?;
        Ok(ExtentLocation {
            fd: c.fd.clone(),
            offset,
            len,
        })
    }

    fn footer(&self, seg: ObjId) -> Result<FooterBody> {
        let c = self.inner.open_container(ContainerKind::Segment, seg)?;
        self.inner.container_footer(&c)
    }

    fn list(&self) -> Result<Vec<SegmentInfo>> {
        let mut out = Vec::new();
        for (raw, size_bytes) in
            list_hex_ids(&self.inner.path("segments"), ".seg").map_err(io_err)?
        {
            if let Ok(seg) = ObjId::from_raw(raw) {
                out.push(SegmentInfo { seg, size_bytes });
            }
        }
        Ok(out)
    }

    fn delete(&self, seg: ObjId) -> Result<u64> {
        self.delete_container(ContainerKind::Segment, seg)
    }
}

impl PackRead for LocalVolume {
    fn read_node(&self, loc: NodeLoc, out: &mut Vec<u8>) -> Result<()> {
        let c = self
            .inner
            .open_container(ContainerKind::MapPack, loc.pack)?;
        let clen = loc
            .len
            .checked_sub(self.inner.ovh)
            .ok_or_else(|| corrupt("map pack", "node length below the overhead"))?;
        self.inner.read_range(&c, loc.off, clen, out)
    }

    fn pack_footer(&self, pack: ObjId) -> Result<FooterBody> {
        let c = self.inner.open_container(ContainerKind::MapPack, pack)?;
        self.inner.container_footer(&c)
    }

    fn list_packs(&self) -> Result<Vec<PackInfo>> {
        let mut out = Vec::new();
        for (raw, size_bytes) in list_hex_ids(&self.inner.path("map"), "").map_err(io_err)? {
            if let Ok(pack) = ObjId::from_raw(raw) {
                out.push(PackInfo { pack, size_bytes });
            }
        }
        Ok(out)
    }
}

impl PackStore for LocalVolume {
    fn open_pack(&self) -> Result<Box<dyn PackWrite>> {
        let w = ContainerWriter::start(self.inner.clone(), ContainerKind::MapPack)?;
        Ok(Box::new(PackWriter(w)))
    }

    fn delete_pack(&self, pack: ObjId) -> Result<u64> {
        self.delete_container(ContainerKind::MapPack, pack)
    }
}

impl LocalVolume {
    /// Re-reads a sealed segment end to end: the footer authenticates, and the
    /// extent region hashes to the footer's `seg_hash`. What `fsck` and tests
    /// use; the hot paths do not pay for it.
    ///
    /// # Errors
    /// `NotFound`, `Corrupt`, `Auth`, `Io`.
    pub fn verify_segment(&self, seg: ObjId) -> Result<()> {
        self.inner.verify_container(ContainerKind::Segment, seg)
    }

    /// As [`verify_segment`](Self::verify_segment) for a map pack.
    ///
    /// # Errors
    /// `NotFound`, `Corrupt`, `Auth`, `Io`.
    pub fn verify_pack(&self, pack: ObjId) -> Result<()> {
        self.inner.verify_container(ContainerKind::MapPack, pack)
    }

    fn delete_container(&self, kind: ContainerKind, id: ObjId) -> Result<u64> {
        self.inner.delete_container(kind, id)
    }
}
