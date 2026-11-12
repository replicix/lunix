//! Map node (freeze item 5, on-disk-formats.md section 8): the leaf codec
//! `rb16`, the interior node, node-hash inputs, empty hashes and tree-shape
//! arithmetic.
//!
//! Nodes are stored as extents (`ExtentKind::Node`); this module is the
//! extent's plaintext payload. Hashing is the caller's: functions here return
//! the exact bytes to hash or take the hash function as a closure.

use crate::bytes::{Put, Reader};
use crate::error::{FormatError, Result};
use crate::ids::{IdLen, IdPrefix, LineageIdx, ObjId, PackId};
use crate::mode::{OVH_ENCRYPTED, OVH_UNENCRYPTED};

const WHAT: &str = "map node";

/// Node header length: `level` u8, `fanout` u16.
pub const HEADER_LEN: usize = 3;
/// Largest encoded node.
pub const MAX_NODE_LEN: usize = 65_536;
/// Highest interior level.
pub const MAX_LEVEL: u8 = 15;
/// Interior child entry length.
pub const CHILD_LEN: usize = 48;
/// Largest `clen` of a data extent a leaf may name (the 1 MiB chunk limit).
pub const MAX_DATA_CLEN: u32 = 1 << 20;
/// Largest whole node extent (`OVH + clen`) an interior entry may name.
pub const MAX_NODE_EXTENT_LEN: u32 = 65_644;

/// Number of address bits of a fanout: 8 for 256, 9 for 512.
pub fn fanout_bits(fanout: u16) -> Result<u32> {
    match fanout {
        256 => Ok(8),
        512 => Ok(9),
        _ => Err(FormatError::range(WHAT, "fanout", u64::from(fanout))),
    }
}

/// The 3-byte node header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NodeHeader {
    /// 0 for a leaf, 1 ..= 15 for an interior node.
    pub level: u8,
    /// 256 or 512, the descriptor's `map_fanout`.
    pub fanout: u16,
}

impl NodeHeader {
    /// Appends the header.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        fanout_bits(self.fanout)?;
        if self.level > MAX_LEVEL {
            return Err(FormatError::range(WHAT, "level", u64::from(self.level)));
        }
        out.put_u8(self.level);
        out.put_u16(self.fanout);
        Ok(())
    }

    /// Reads a header.
    pub fn read(r: &mut Reader<'_>) -> Result<Self> {
        let level = r.u8()?;
        let fanout = r.u16()?;
        fanout_bits(fanout)?;
        if level > MAX_LEVEL {
            return Err(FormatError::range(WHAT, "level", u64::from(level)));
        }
        Ok(Self { level, fanout })
    }
}

fn bit_width(v: u64) -> u32 {
    64 - v.leading_zeros()
}

// ---------------------------------------------------------------------------
// Bit packing (LSB first).

struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u128,
    n: u32,
}

impl<'a> BitWriter<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out, acc: 0, n: 0 }
    }

    fn put(&mut self, v: u64, w: u32) {
        if w == 0 {
            return;
        }
        self.acc |= u128::from(v) << self.n;
        self.n += w;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Ends a column on a byte boundary, high bits zero.
    fn align(&mut self) {
        if self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc = 0;
            self.n = 0;
        }
    }
}

fn get_bits(buf: &[u8], bit: usize, w: u32) -> Result<u64> {
    if w == 0 {
        return Ok(0);
    }
    let by = bit / 8;
    let sh = (bit % 8) as u32;
    let nbytes = ((sh + w) as usize).div_ceil(8);
    let end = by
        .checked_add(nbytes)
        .ok_or(FormatError::invalid(WHAT, "bit offset overflow"))?;
    let s = buf.get(by..end).ok_or(FormatError::Truncated {
        what: WHAT,
        need: end,
        have: buf.len(),
    })?;
    let mut acc: u128 = 0;
    for (i, b) in s.iter().enumerate() {
        acc |= u128::from(*b) << (8 * i);
    }
    let v = (acc >> sh) as u64;
    Ok(if w == 64 { v } else { v & ((1u64 << w) - 1) })
}

// ---------------------------------------------------------------------------
// Leaf.

/// What a leaf codec needs from the descriptor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LeafParams {
    /// `map_fanout`: 256 or 512.
    pub fanout: u16,
    /// Id-prefix length.
    pub id_len: IdLen,
    /// Extent overhead: 68 (plain, integrity) or 108 (encrypted).
    pub ovh: u32,
}

impl LeafParams {
    fn sb(&self) -> Result<u32> {
        if self.ovh != OVH_UNENCRYPTED && self.ovh != OVH_ENCRYPTED {
            return Err(FormatError::range(WHAT, "ovh", u64::from(self.ovh)));
        }
        fanout_bits(self.fanout)
    }
}

/// One non-hole leaf entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DataEntry {
    /// 0 in v1.
    pub lineage: LineageIdx,
    /// The segment holding the extent.
    pub seg: ObjId,
    /// Offset of the extent in the segment.
    pub off: u32,
    /// Payload length of the extent.
    pub clen: u32,
    /// The first `id_len` bytes of the chunk id.
    pub prefix: IdPrefix,
}

/// A leaf slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    /// Unwritten (reads as zeros).
    Hole,
    /// A stored chunk.
    Data(DataEntry),
}

/// A leaf: exactly `fanout` entries. An all-hole leaf encodes to the 25-byte
/// empty leaf, which stores must never write (they reference the empty hash
/// with a zero location instead).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Leaf {
    /// The entries, `fanout` of them.
    pub entries: Vec<Entry>,
}

type RunKey = Option<(u8, u64)>;

impl Leaf {
    /// A leaf of `fanout` holes.
    pub fn empty(fanout: u16) -> Self {
        Self {
            entries: vec![Entry::Hole; usize::from(fanout)],
        }
    }

    /// True if every entry is a hole.
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|e| *e == Entry::Hole)
    }

    /// Appends the canonical `rb16` encoding. Non-zero `lineage` is refused
    /// (v1).
    pub fn encode(&self, p: &LeafParams, out: &mut Vec<u8>) -> Result<()> {
        encode_leaf(&self.entries, p, false, out)
    }

    /// Decodes a leaf, requiring the canonical encoding and zero lineage.
    pub fn decode(buf: &[u8], p: &LeafParams) -> Result<Self> {
        decode_leaf(buf, p, false)
    }

    /// Like [`Leaf::encode`] but allows a non-zero `lineage` (ADR-25, post-v1).
    #[doc(hidden)]
    pub fn encode_allow_lineage(&self, p: &LeafParams, out: &mut Vec<u8>) -> Result<()> {
        encode_leaf(&self.entries, p, true, out)
    }

    /// Like [`Leaf::decode`] but allows a non-zero `lineage`.
    #[doc(hidden)]
    pub fn decode_allow_lineage(buf: &[u8], p: &LeafParams) -> Result<Self> {
        decode_leaf(buf, p, true)
    }
}

/// The 25-byte encoding of the empty leaf of `fanout`.
pub fn empty_leaf_bytes(fanout: u16) -> Result<Vec<u8>> {
    let p = LeafParams {
        fanout,
        id_len: IdLen::Zero,
        ovh: OVH_UNENCRYPTED,
    };
    let mut v = Vec::with_capacity(25);
    Leaf::empty(fanout).encode(&p, &mut v)?;
    Ok(v)
}

fn encode_leaf(
    entries: &[Entry],
    p: &LeafParams,
    allow_lineage: bool,
    out: &mut Vec<u8>,
) -> Result<()> {
    let sb = p.sb()?;
    let fanout = usize::from(p.fanout);
    if entries.len() != fanout {
        return Err(FormatError::invalid(
            WHAT,
            "a leaf has exactly fanout entries",
        ));
    }
    let ovh = u64::from(p.ovh);
    // Runs: (start, key, start_off, rank).
    let mut runs: Vec<(usize, RunKey, u32, usize)> = Vec::new();
    let mut ndata = 0usize;
    let mut prev: Option<&Entry> = None;
    for (i, e) in entries.iter().enumerate() {
        if let Entry::Data(d) = e {
            if d.clen == 0 || d.clen > MAX_DATA_CLEN {
                return Err(FormatError::range(WHAT, "clen", u64::from(d.clen)));
            }
            if d.prefix.id_len() != p.id_len {
                return Err(FormatError::invalid(
                    WHAT,
                    "id prefix length differs from id_len",
                ));
            }
            if d.lineage.0 != 0 && !allow_lineage {
                return Err(FormatError::range(
                    WHAT,
                    "lineage_idx",
                    u64::from(d.lineage.0),
                ));
            }
        }
        let cont = match (prev, e) {
            (Some(Entry::Hole), Entry::Hole) => true,
            (Some(Entry::Data(a)), Entry::Data(b)) => {
                a.lineage == b.lineage
                    && a.seg == b.seg
                    && u64::from(b.off) == u64::from(a.off) + ovh + u64::from(a.clen)
            }
            _ => false,
        };
        if !cont {
            match e {
                Entry::Hole => runs.push((i, None, 0, ndata)),
                Entry::Data(d) => runs.push((i, Some((d.lineage.0, d.seg.raw())), d.off, ndata)),
            }
        }
        if matches!(e, Entry::Data(_)) {
            ndata += 1;
        }
        prev = Some(e);
    }
    let mut dict: Vec<(u8, u64)> = runs.iter().filter_map(|r| r.1).collect();
    dict.sort_unstable();
    dict.dedup();
    let ndict = dict.len();
    let base = dict.iter().map(|d| d.1).min().unwrap_or(0);
    let max_sd = dict.iter().map(|d| d.1 - base).max().unwrap_or(0);
    let wsd = bit_width(max_sd);
    let wseg = bit_width(ndict as u64);
    let woff = bit_width(u64::from(
        runs.iter()
            .filter(|r| r.1.is_some())
            .map(|r| r.2)
            .max()
            .unwrap_or(0),
    ));
    let clens: Vec<u32> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Data(d) => Some(d.clen),
            Entry::Hole => None,
        })
        .collect();
    let wc = bit_width(u64::from(clens.iter().copied().max().unwrap_or(0)));
    let mut cum: Vec<u64> = Vec::with_capacity(clens.len().div_ceil(16));
    let mut s = 0u64;
    for (j, c) in clens.iter().enumerate() {
        if j % 16 == 0 {
            cum.push(s);
        }
        s += ovh + u64::from(*c);
    }
    let wcum = bit_width(cum.last().copied().unwrap_or(0));

    NodeHeader {
        level: 0,
        fanout: p.fanout,
    }
    .encode(out)?;
    out.put_u16(runs.len() as u16);
    out.put_u16(ndata as u16);
    out.put_u16(ndict as u16);
    for w in [wseg, wsd, woff, wc, wcum] {
        out.put_u8(w as u8);
    }
    out.put_u64(base);
    let mut w = BitWriter::new(out);
    for d in &dict {
        w.put(d.1 - base, wsd);
    }
    w.align();
    for d in &dict {
        w.out.put_u8(d.0);
    }
    for r in &runs {
        let si = match r.1 {
            None => 0,
            Some(k) => dict.binary_search(&k).map_or(0, |i| i as u64 + 1),
        };
        w.put(r.0 as u64, sb);
        w.put(si, wseg);
        w.put(u64::from(if r.1.is_some() { r.2 } else { 0 }), woff);
        w.put(r.3 as u64, sb + 1);
    }
    w.align();
    for c in &clens {
        w.put(u64::from(*c), wc);
    }
    w.align();
    for c in &cum {
        w.put(*c, wcum);
    }
    w.align();
    for e in entries {
        if let Entry::Data(d) = e {
            out.extend_from_slice(d.prefix.as_bytes());
        }
    }
    if out.len() > MAX_NODE_LEN {
        return Err(FormatError::invalid(WHAT, "encoded node over 64 KiB"));
    }
    Ok(())
}

/// Parsed fixed part of a leaf with the byte offset of every column.
struct Layout {
    fanout: usize,
    sb: u32,
    ovh: u64,
    id_len: usize,
    nruns: usize,
    ndata: usize,
    ndict: usize,
    wseg: u32,
    wsd: u32,
    woff: u32,
    wc: u32,
    wcum: u32,
    base: u64,
    rw: usize,
    dict_o: usize,
    lin_o: usize,
    runs_o: usize,
    clen_o: usize,
    cum_o: usize,
    ids_o: usize,
}

fn parse_layout(buf: &[u8], p: &LeafParams) -> Result<Layout> {
    let sb = p.sb()?;
    let mut r = Reader::new(buf, WHAT);
    let h = NodeHeader::read(&mut r)?;
    if h.level != 0 || h.fanout != p.fanout {
        return Err(FormatError::invalid(WHAT, "not a leaf of this fanout"));
    }
    let fanout = usize::from(p.fanout);
    let nruns = usize::from(r.u16()?);
    let ndata = usize::from(r.u16()?);
    let ndict = usize::from(r.u16()?);
    let wseg = u32::from(r.u8()?);
    let wsd = u32::from(r.u8()?);
    let woff = u32::from(r.u8()?);
    let wc = u32::from(r.u8()?);
    let wcum = u32::from(r.u8()?);
    let base = r.u64()?;
    if nruns == 0 || nruns > fanout {
        return Err(FormatError::count(
            WHAT,
            "nruns",
            nruns as u64,
            fanout as u64,
        ));
    }
    if ndata > fanout {
        return Err(FormatError::count(
            WHAT,
            "ndata",
            ndata as u64,
            fanout as u64,
        ));
    }
    if ndict > ndata {
        return Err(FormatError::count(
            WHAT,
            "ndict",
            ndict as u64,
            ndata as u64,
        ));
    }
    if wseg != bit_width(ndict as u64) || wsd > 64 || woff > 32 || wc > 32 || wcum > 48 {
        return Err(FormatError::invalid(WHAT, "column width out of range"));
    }
    let rw = (sb + wseg + woff + sb + 1) as usize;
    let dict_o = r.pos();
    let lin_o = dict_o + (ndict * wsd as usize).div_ceil(8);
    let runs_o = lin_o + ndict;
    let clen_o = runs_o + (nruns * rw).div_ceil(8);
    let cum_o = clen_o + (ndata * wc as usize).div_ceil(8);
    let ids_o = cum_o + (ndata.div_ceil(16) * wcum as usize).div_ceil(8);
    let total = ids_o + ndata * p.id_len.len();
    if buf.len() < total {
        return Err(FormatError::Truncated {
            what: WHAT,
            need: total,
            have: buf.len(),
        });
    }
    if buf.len() > total {
        return Err(FormatError::Trailing {
            what: WHAT,
            extra: buf.len() - total,
        });
    }
    Ok(Layout {
        fanout,
        sb,
        ovh: u64::from(p.ovh),
        id_len: p.id_len.len(),
        nruns,
        ndata,
        ndict,
        wseg,
        wsd,
        woff,
        wc,
        wcum,
        base,
        rw,
        dict_o,
        lin_o,
        runs_o,
        clen_o,
        cum_o,
        ids_o,
    })
}

impl Layout {
    /// `(start, seg_idx, start_off, rank)` of run `r`.
    fn run(&self, buf: &[u8], r: usize) -> Result<(usize, u64, u64, usize)> {
        let at = (self.runs_o * 8)
            .checked_add(r * self.rw)
            .ok_or(FormatError::invalid(WHAT, "run offset overflow"))?;
        let start = get_bits(buf, at, self.sb)? as usize;
        let si = get_bits(buf, at + self.sb as usize, self.wseg)?;
        let off = get_bits(buf, at + (self.sb + self.wseg) as usize, self.woff)?;
        let rank = get_bits(
            buf,
            at + (self.sb + self.wseg + self.woff) as usize,
            self.sb + 1,
        )?;
        Ok((start, si, off, rank as usize))
    }

    fn clen(&self, buf: &[u8], k: usize) -> Result<u32> {
        if k >= self.ndata {
            return Err(FormatError::invalid(WHAT, "data index past ndata"));
        }
        Ok(get_bits(buf, self.clen_o * 8 + k * self.wc as usize, self.wc)? as u32)
    }

    fn prefix(&self, buf: &[u8], k: usize, id_len: IdLen) -> Result<IdPrefix> {
        let from = self.ids_o + k * self.id_len;
        let s = buf
            .get(from..from + self.id_len)
            .ok_or(FormatError::invalid(WHAT, "id column out of range"))?;
        IdPrefix::from_slice(id_len, s)
    }

    fn dict_entry(&self, buf: &[u8], i: usize) -> Result<(u8, ObjId)> {
        if i >= self.ndict {
            return Err(FormatError::invalid(WHAT, "segment index past ndict"));
        }
        let v = get_bits(buf, self.dict_o * 8 + i * self.wsd as usize, self.wsd)?;
        let seg = self
            .base
            .checked_add(v)
            .ok_or(FormatError::invalid(WHAT, "segment id overflow"))?;
        let lineage = *buf
            .get(self.lin_o + i)
            .ok_or(FormatError::invalid(WHAT, "lineage column out of range"))?;
        Ok((lineage, ObjId::from_raw(seg)?))
    }

    /// `S(k)`: bytes of the first `k` non-hole extents of the leaf.
    fn s_of(&self, buf: &[u8], k: usize) -> Result<u64> {
        let g = k / 16;
        let mut s = get_bits(buf, self.cum_o * 8 + g * self.wcum as usize, self.wcum)?;
        for j in g * 16..k {
            s = s
                .checked_add(self.ovh + u64::from(self.clen(buf, j)?))
                .ok_or(FormatError::invalid(WHAT, "offset overflow"))?;
        }
        Ok(s)
    }
}

fn decode_leaf(buf: &[u8], p: &LeafParams, allow_lineage: bool) -> Result<Leaf> {
    let l = parse_layout(buf, p)?;
    let mut dict: Vec<(u8, ObjId)> = Vec::with_capacity(l.ndict);
    for i in 0..l.ndict {
        dict.push(l.dict_entry(buf, i)?);
    }
    if !allow_lineage && dict.iter().any(|d| d.0 != 0) {
        return Err(FormatError::range(WHAT, "lineage_idx", 1));
    }
    let mut entries: Vec<Entry> = Vec::with_capacity(l.fanout);
    let mut k = 0usize;
    for r in 0..l.nruns {
        let (start, si, start_off, rank) = l.run(buf, r)?;
        let end = if r + 1 < l.nruns {
            l.run(buf, r + 1)?.0
        } else {
            l.fanout
        };
        if start != entries.len() || end <= start || end > l.fanout {
            return Err(FormatError::not_canonical(WHAT, "run starts"));
        }
        if rank != k {
            return Err(FormatError::not_canonical(WHAT, "run rank"));
        }
        if si == 0 {
            if start_off != 0 {
                return Err(FormatError::not_canonical(WHAT, "hole run offset"));
            }
            entries.resize(end, Entry::Hole);
            continue;
        }
        let (lineage, seg) = *dict
            .get(si as usize - 1)
            .ok_or(FormatError::invalid(WHAT, "segment index past ndict"))?;
        let mut off = start_off;
        for _ in start..end {
            let clen = l.clen(buf, k)?;
            let off32 = u32::try_from(off).map_err(|_| FormatError::Overflow {
                what: WHAT,
                why: "extent offset",
            })?;
            entries.push(Entry::Data(DataEntry {
                lineage: LineageIdx(lineage),
                seg,
                off: off32,
                clen,
                prefix: l.prefix(buf, k, p.id_len)?,
            }));
            off += l.ovh + u64::from(clen);
            k += 1;
        }
    }
    if k != l.ndata || entries.len() != l.fanout {
        return Err(FormatError::invalid(WHAT, "entry count mismatch"));
    }
    let mut again = Vec::with_capacity(buf.len());
    encode_leaf(&entries, p, allow_lineage, &mut again)?;
    if again != buf {
        return Err(FormatError::not_canonical(
            WHAT,
            "leaf is not the canonical encoding",
        ));
    }
    Ok(Leaf { entries })
}

/// Random-access lookup in an encoded leaf without decoding it.
///
/// The view validates sizes and bounds but not canonical form: use it on
/// buffers that were authenticated or decoded once with [`Leaf::decode`].
/// It never panics on arbitrary input.
pub struct LeafView<'a> {
    buf: &'a [u8],
    lay: Layout,
    id_len: IdLen,
}

impl<'a> LeafView<'a> {
    /// Checks the fixed part and column sizes.
    pub fn parse(buf: &'a [u8], p: &LeafParams) -> Result<Self> {
        Ok(Self {
            buf,
            lay: parse_layout(buf, p)?,
            id_len: p.id_len,
        })
    }

    /// Entry `i` (0 ..= fanout - 1).
    pub fn get(&self, i: usize) -> Result<Entry> {
        let l = &self.lay;
        if i >= l.fanout {
            return Err(FormatError::range(WHAT, "entry index", i as u64));
        }
        let (mut lo, mut hi) = (0usize, l.nruns);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if l.run(self.buf, mid)?.0 <= i {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (start, si, start_off, rank) = l.run(self.buf, lo)?;
        if start > i {
            return Err(FormatError::invalid(WHAT, "first run does not start at 0"));
        }
        if si == 0 {
            return Ok(Entry::Hole);
        }
        let (lineage, seg) = l.dict_entry(self.buf, si as usize - 1)?;
        let k = rank
            .checked_add(i - start)
            .ok_or(FormatError::invalid(WHAT, "rank overflow"))?;
        let clen = l.clen(self.buf, k)?;
        let delta = l
            .s_of(self.buf, k)?
            .checked_sub(l.s_of(self.buf, rank)?)
            .ok_or(FormatError::invalid(WHAT, "cum column not monotone"))?;
        let off = start_off
            .checked_add(delta)
            .and_then(|o| u32::try_from(o).ok())
            .ok_or(FormatError::Overflow {
                what: WHAT,
                why: "extent offset",
            })?;
        Ok(Entry::Data(DataEntry {
            lineage: LineageIdx(lineage),
            seg,
            off,
            clen,
            prefix: l.prefix(self.buf, k, self.id_len)?,
        }))
    }
}

// ---------------------------------------------------------------------------
// Interior node.

/// Location of a child node's extent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NodeLoc {
    /// The map pack.
    pub pack: PackId,
    /// Offset of the extent in the pack.
    pub off: u32,
    /// Whole extent length (`OVH + clen`), 1 ..= 65 644.
    pub len: u32,
}

/// One interior child entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Child {
    /// The child's node hash (the empty hash of the level below for an
    /// absent subtree).
    pub hash: [u8; 32],
    /// Where the child is stored; `None` for an empty child.
    pub loc: Option<NodeLoc>,
}

/// An interior node: `level` (1 ..= 15) and `fanout` children.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Interior {
    /// The node level.
    pub level: u8,
    /// Exactly `fanout` children.
    pub children: Vec<Child>,
}

impl Interior {
    /// Appends the encoding. `lineage_idx` is written as 0 (v1).
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let fanout = u16::try_from(self.children.len())
            .map_err(|_| FormatError::range(WHAT, "fanout", self.children.len() as u64))?;
        if self.level == 0 {
            return Err(FormatError::range(WHAT, "level", 0));
        }
        out.reserve(HEADER_LEN + CHILD_LEN * self.children.len());
        NodeHeader {
            level: self.level,
            fanout,
        }
        .encode(out)?;
        for c in &self.children {
            out.extend_from_slice(&c.hash);
            match c.loc {
                None => out.put_zeros(16),
                Some(l) => {
                    if l.len == 0 || l.len > MAX_NODE_EXTENT_LEN {
                        return Err(FormatError::range(WHAT, "len", u64::from(l.len)));
                    }
                    out.put_u64(l.pack.raw());
                    out.put_u32(l.off);
                    out.put_u24(l.len);
                    out.put_u8(0);
                }
            }
        }
        Ok(())
    }

    /// Decodes an interior node of the descriptor's `fanout`.
    pub fn decode(buf: &[u8], fanout: u16) -> Result<Self> {
        fanout_bits(fanout)?;
        let mut r = Reader::new(buf, WHAT);
        let h = NodeHeader::read(&mut r)?;
        if h.level == 0 || h.fanout != fanout {
            return Err(FormatError::invalid(
                WHAT,
                "not an interior node of this fanout",
            ));
        }
        let n = r.check_count("children", u64::from(fanout), u64::from(fanout), CHILD_LEN)?;
        let mut children = Vec::with_capacity(n);
        for _ in 0..n {
            let hash = r.array::<32>()?;
            let pack = r.u64()?;
            let off = r.u32()?;
            let len = r.u24()?;
            let lineage = r.u8()?;
            if lineage != 0 {
                return Err(FormatError::range(WHAT, "lineage_idx", u64::from(lineage)));
            }
            let loc = match ObjId::from_raw_opt(pack)? {
                None => {
                    if off != 0 || len != 0 {
                        return Err(FormatError::invalid(WHAT, "empty child with a location"));
                    }
                    None
                }
                Some(pack) => {
                    if len == 0 || len > MAX_NODE_EXTENT_LEN {
                        return Err(FormatError::range(WHAT, "len", u64::from(len)));
                    }
                    Some(NodeLoc { pack, off, len })
                }
            };
            children.push(Child { hash, loc });
        }
        r.finish()?;
        Ok(Self {
            level: h.level,
            children,
        })
    }

    /// The bytes the node hash covers: header ‖ child hashes, no locations.
    pub fn hash_input(&self) -> Result<Vec<u8>> {
        let hashes: Vec<[u8; 32]> = self.children.iter().map(|c| c.hash).collect();
        let fanout = u16::try_from(hashes.len())
            .map_err(|_| FormatError::range(WHAT, "fanout", hashes.len() as u64))?;
        interior_hash_input(self.level, fanout, &hashes)
    }
}

/// `header(level, fanout) ‖ child_hash × fanout` (section 8.5).
pub fn interior_hash_input(level: u8, fanout: u16, child_hashes: &[[u8; 32]]) -> Result<Vec<u8>> {
    if level == 0 || child_hashes.len() != usize::from(fanout) {
        return Err(FormatError::invalid(WHAT, "interior hash input shape"));
    }
    let mut v = Vec::with_capacity(HEADER_LEN + 32 * child_hashes.len());
    NodeHeader { level, fanout }.encode(&mut v)?;
    for h in child_hashes {
        v.extend_from_slice(h);
    }
    Ok(v)
}

/// The leaf hash input is the whole leaf encoding; this exists for symmetry
/// and returns its argument.
pub fn leaf_hash_input(leaf_bytes: &[u8]) -> &[u8] {
    leaf_bytes
}

/// `E0 ..= E_max_level`: `E0 = h(empty leaf)`,
/// `E_L = h(header(L, fanout) ‖ E_{L-1} × fanout)`.
pub fn empty_hashes<H: Fn(&[u8]) -> [u8; 32]>(
    fanout: u16,
    max_level: u8,
    h: H,
) -> Result<Vec<[u8; 32]>> {
    if max_level > MAX_LEVEL {
        return Err(FormatError::range(WHAT, "level", u64::from(max_level)));
    }
    let mut v = vec![h(&empty_leaf_bytes(fanout)?)];
    for level in 1..=max_level {
        let below = v[usize::from(level) - 1];
        let input = interior_hash_input(level, fanout, &vec![below; usize::from(fanout)])?;
        v.push(h(&input));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Tree shape.

/// `max(2, ceil(log_fanout(nchunks)))` with `nchunks = ceil(size / chunk_size)`.
pub fn levels_for(size: u64, chunk_size: u32, fanout: u16) -> Result<u8> {
    fanout_bits(fanout)?;
    if chunk_size == 0 {
        return Err(FormatError::range(WHAT, "chunk_size", 0));
    }
    let nchunks = u128::from(size.div_ceil(u64::from(chunk_size)));
    let mut cap: u128 = 1;
    let mut levels: u8 = 0;
    while cap < nchunks {
        cap *= u128::from(fanout);
        levels += 1;
        if levels > MAX_LEVEL {
            return Err(FormatError::range(WHAT, "size", size));
        }
    }
    Ok(levels.max(2))
}

/// Leaf index of chunk `chunk_index`.
pub fn leaf_index(chunk_index: u64, fanout: u16) -> u64 {
    chunk_index / u64::from(fanout.max(1))
}

/// Slot of chunk `chunk_index` in its leaf.
pub fn leaf_slot(chunk_index: u64, fanout: u16) -> usize {
    (chunk_index % u64::from(fanout.max(1))) as usize
}

/// Child slot at interior `level` (>= 1) on the path to leaf `leaf_index`:
/// `(leaf_index / fanout^(level-1)) % fanout`.
pub fn child_slot(leaf_index: u64, level: u8, fanout: u16) -> Result<usize> {
    fanout_bits(fanout)?;
    if level == 0 || level > MAX_LEVEL {
        return Err(FormatError::range(WHAT, "level", u64::from(level)));
    }
    let f = u64::from(fanout);
    Ok(match f.checked_pow(u32::from(level) - 1) {
        Some(div) => ((leaf_index / div) % f) as usize,
        None => 0,
    })
}
