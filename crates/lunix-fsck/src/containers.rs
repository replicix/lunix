//! Step 4: segments and map packs (R3): trailer, footer, `seg_hash`, a scan of
//! the extents against the footer and, with `--deep`, every extent opened.

use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::Path;

use lunix_format::extent::CCH1_MAGIC;
use lunix_format::segment::{
    decode_footer_frame, footer_body_cap, ContainerKind, Trailer, MAX_NODE_ULEN, TRAILER_LEN,
};
use lunix_format::{paths, ChunkId, Codec, ExtentKind, ObjId, ObjectKind};
use lunix_iface::crypto::{OpenExpect, OpenOutput};
use lunix_iface::ErrorKind;

use crate::code;
use crate::facts::Facts;
use crate::report::{Finding, Sink};
use crate::session::Session;

/// One extent as the authenticated footer lists it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ext {
    pub(crate) off: u32,
    pub(crate) clen: u32,
    pub(crate) ulen: u32,
    pub(crate) codec: Codec,
    pub(crate) id: ChunkId,
    /// A retained root resolved an entry to this extent (set by `resolve`).
    pub(crate) live: bool,
}

/// A sealed container as fsck found it.
#[derive(Clone, Debug)]
pub(crate) struct Container {
    pub(crate) size: u64,
    /// The footer authenticated and decoded (R3): only then is `exts` filled
    /// and the container used at all.
    pub(crate) good: bool,
    pub(crate) extents_len: u64,
    /// Sorted by offset.
    pub(crate) exts: Vec<Ext>,
}

impl Container {
    pub(crate) fn find(&self, off: u32) -> Option<&Ext> {
        self.exts
            .binary_search_by_key(&off, |e| e.off)
            .ok()
            .map(|i| &self.exts[i])
    }
}

pub(crate) fn parse_hex16(name: &str) -> Option<u64> {
    if name.len() == 16
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        u64::from_str_radix(name, 16).ok()
    } else {
        None
    }
}

fn names(dir: &Path, rel: &str, sink: &mut Sink) -> Vec<String> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            sink.push(Finding::new(code::IO, rel, format!("cannot list: {e}")));
            return Vec::new();
        }
    };
    let mut v: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    v.sort();
    v
}

/// No footer is longer than this, whatever the trailer claims.
const MAX_FOOTER_LEN: u64 = 256 << 20;

pub(crate) fn check(s: &Session, deep: bool, sink: &mut Sink, facts: &mut Facts) {
    for name in names(&s.dir.join("segments"), "segments/", sink) {
        let Some(stem) = name.strip_suffix(".seg") else {
            continue;
        };
        let Some(id) = parse_hex16(stem).and_then(|r| ObjId::from_raw(r).ok()) else {
            continue;
        };
        let c = verify(s, ContainerKind::Segment, id, deep, sink, facts);
        facts.stats.segments += 1;
        facts.segments.insert(id, c);
    }
    for name in names(&s.dir.join("map"), "map/", sink) {
        let Some(id) = parse_hex16(&name).and_then(|r| ObjId::from_raw(r).ok()) else {
            continue;
        };
        let c = verify(s, ContainerKind::MapPack, id, deep, sink, facts);
        facts.stats.packs += 1;
        facts.packs.insert(id, c);
    }
}

fn rel(kind: ContainerKind, id: ObjId) -> String {
    match kind {
        ContainerKind::Segment => format!("segments/{:016x}.seg", id.raw()),
        ContainerKind::MapPack => format!("map/{:016x}", id.raw()),
    }
}

fn verify(
    s: &Session,
    kind: ContainerKind,
    id: ObjId,
    deep: bool,
    sink: &mut Sink,
    facts: &mut Facts,
) -> Container {
    let obj = rel(kind, id);
    let mut c = Container {
        size: 0,
        good: false,
        extents_len: 0,
        exts: Vec::new(),
    };
    let file = match File::open(s.dir.join(&obj)) {
        Ok(f) => f,
        Err(e) => {
            sink.push(Finding::new(code::IO, &obj, format!("cannot open: {e}")));
            return c;
        }
    };
    let size = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => {
            sink.push(Finding::new(code::IO, &obj, format!("cannot stat: {e}")));
            return c;
        }
    };
    c.size = size;
    let trailer_err = |sink: &mut Sink, why: String| {
        sink.push(Finding::new(code::CONTAINER_TRAILER, &obj, why));
    };
    let Some(tail_off) = size.checked_sub(TRAILER_LEN as u64) else {
        trailer_err(sink, "the file is shorter than a trailer".to_owned());
        return c;
    };
    let mut tail = [0u8; TRAILER_LEN];
    if let Err(e) = file.read_exact_at(&mut tail, tail_off) {
        sink.push(Finding::new(code::IO, &obj, format!("cannot read: {e}")));
        return c;
    }
    let trailer = match Trailer::decode(&tail) {
        Ok(t) => t,
        Err(e) => {
            trailer_err(sink, format!("{e}"));
            return c;
        }
    };
    let total = trailer
        .footer_off
        .checked_add(u64::from(trailer.footer_len))
        .and_then(|v| v.checked_add(TRAILER_LEN as u64));
    if total != Some(size) {
        trailer_err(sink, "the trailer does not match the file size".to_owned());
        return c;
    }
    if trailer.object_id != id || trailer.container_kind != kind {
        trailer_err(sink, "the trailer names another object".to_owned());
        return c;
    }

    // R3: the footer is used only after it authenticates under this name.
    let Ok(cap) = footer_body_cap(trailer.footer_off) else {
        trailer_err(sink, "the extent region length is out of range".to_owned());
        return c;
    };
    let cap = cap as u64;
    if u64::from(trailer.footer_len) > (cap + cap / 128 + 1024).min(MAX_FOOTER_LEN) {
        sink.push(Finding::new(
            code::FOOTER_CORRUPT,
            &obj,
            "the footer is longer than any footer of this region",
        ));
        return c;
    }
    let mut env = vec![0u8; trailer.footer_len as usize];
    if let Err(e) = file.read_exact_at(&mut env, trailer.footer_off) {
        sink.push(Finding::new(code::IO, &obj, format!("cannot read: {e}")));
        return c;
    }
    let bucket_name = match kind {
        ContainerKind::Segment => paths::segment(id),
        ContainerKind::MapPack => paths::map_pack(id),
    };
    let frame = match s.meta_codec().open(ObjectKind::Footer, &bucket_name, &env) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::Auth => {
            sink.push(Finding::new(
                code::FOOTER_AUTH,
                &obj,
                "the footer does not authenticate under this name; the container is treated as absent",
            ));
            return c;
        }
        Err(e) => {
            sink.push(Finding::new(code::FOOTER_CORRUPT, &obj, format!("{e}")));
            return c;
        }
    };
    let body = match decode_footer_frame(&frame, trailer.footer_off) {
        Ok(b) => b,
        Err(e) => {
            sink.push(Finding::new(code::FOOTER_CORRUPT, &obj, format!("{e}")));
            return c;
        }
    };
    if let Err(e) = body
        .check_trailer(&trailer)
        .and_then(|()| body.check_tiling(s.ovh()))
    {
        sink.push(Finding::new(code::FOOTER_CORRUPT, &obj, format!("{e}")));
        return c;
    }
    c.good = true;
    c.extents_len = body.extents_len;
    c.exts = body
        .entries
        .iter()
        .map(|e| Ext {
            off: e.off,
            clen: e.clen,
            ulen: e.ulen,
            codec: e.codec,
            id: e.chunk_id,
            live: false,
        })
        .collect();
    c.exts.sort_unstable_by_key(|e| e.off);

    // The region, hashed whole (R3) and scanned against the footer.
    let Ok(region_len) = usize::try_from(trailer.footer_off) else {
        trailer_err(sink, "the extent region does not fit memory".to_owned());
        return c;
    };
    let mut region = vec![0u8; region_len];
    if let Err(e) = file.read_exact_at(&mut region, 0) {
        sink.push(Finding::new(code::IO, &obj, format!("cannot read: {e}")));
        return c;
    }
    if s.commit_codec().object_hash(&region) != body.seg_hash {
        sink.push(Finding::new(
            code::SEG_HASH,
            &obj,
            "the extent region does not hash to the footer's seg_hash",
        ));
    }
    let ovh = s.ovh() as usize;
    for e in &c.exts {
        let start = e.off as usize;
        let end = start + ovh + e.clen as usize;
        let ok = region.get(start..end).is_some_and(|b| {
            b[0..4] == *CCH1_MAGIC
                && b[4] == 1
                && u16::from_le_bytes([b[5], b[6]]) == e.codec.id()
                && u64::from_le_bytes(b[8..16].try_into().unwrap_or([0; 8])) == u64::from(e.ulen)
                && b[16..48] == e.id.0
                && u32::from_le_bytes(b[48..52].try_into().unwrap_or([0; 4])) == e.clen
        });
        facts.stats.extents += 1;
        if !ok {
            sink.push(
                Finding::new(
                    code::FOOTER_ENTRIES,
                    &obj,
                    "the extent found at this offset differs from its footer entry",
                )
                .with_range(start as u64, end as u64),
            );
        }
    }
    if deep {
        deep_open(s, kind, &c, &region, &obj, sink);
    }
    c
}

fn deep_open(
    s: &Session,
    kind: ContainerKind,
    c: &Container,
    region: &[u8],
    obj: &str,
    sink: &mut Sink,
) {
    let ovh = s.ovh() as usize;
    let (xk, cap) = match kind {
        ContainerKind::Segment => (ExtentKind::Data, s.desc.chunk_size),
        ContainerKind::MapPack => (ExtentKind::Node, MAX_NODE_ULEN),
    };
    let expect = OpenExpect {
        chunk_size: cap,
        chunk_id: None,
        id_prefix: None,
        // Data extents are scans: recompute the id (R1). A node's id is the
        // hash of its hash input, which the map walk recomputes.
        verify_id: kind == ContainerKind::Segment,
        output: OpenOutput::Plaintext,
    };
    let mut out = Vec::new();
    for e in &c.exts {
        let start = e.off as usize;
        let end = start + ovh + e.clen as usize;
        let Some(bytes) = region.get(start..end) else {
            continue;
        };
        out.clear();
        match s.extents().open(xk, bytes, &expect, &mut out) {
            Ok(meta) => {
                if meta.chunk_id != e.id {
                    sink.push(
                        Finding::new(
                            code::EXTENT_ID,
                            obj,
                            "the extent's header id differs from its footer entry",
                        )
                        .with_range(start as u64, end as u64),
                    );
                }
            }
            Err(err) => {
                let why = if err.kind() == ErrorKind::Auth {
                    "the tag does not verify, or the recomputed id differs from the header id"
                } else {
                    "the extent does not decode"
                };
                sink.push(
                    Finding::new(code::EXTENT_AUTH, obj, why).with_range(start as u64, end as u64),
                );
            }
        }
    }
}
