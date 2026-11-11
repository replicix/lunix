//! Leaf codecs for the radix map spike.
//!
//! Derived from `spikes/leafenc/src/codec.rs` (`Rb` = run-encoded, bit-packed
//! columns, 16-entry restart sums; `Row` = plain fixed rows). Changes versus
//! the original: runtime power-of-two fanout (the run record widths follow
//! `log2(fan)`), an owned 48-byte `Ent` instead of the generator's `E`, and no
//! slot/varint/column variants. The bit layout of `Rb` is otherwise unchanged.
use crate::gen::HDR;

/// One map entry as the spike sees it: 32-byte chunk id (only the first `t`
/// bytes are stored in a leaf), segment, offset, compressed length. 48 bytes.
/// `seg == 0` is a hole.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Ent {
    pub id: [u8; 32],
    pub seg: u64,
    pub off: u32,
    pub clen: u32,
}
impl Ent {
    #[inline]
    pub fn hole(&self) -> bool {
        self.seg == 0
    }
}

/// Result of a point lookup on an encoded leaf (`idp` = first 8 id bytes).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Lk {
    pub seg: u64,
    pub off: u32,
    pub clen: u32,
    pub idp: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    None,
    Lz4,
    Zstd1,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Runs,
    Rows,
}

/// Leaf format: codec kind, stored id length `t`, outer compression, fanout.
#[derive(Clone, Copy, Debug)]
pub struct LeafFmt {
    pub kind: Kind,
    pub t: usize,
    pub wrap: Wrap,
    pub fan: usize,
}

impl LeafFmt {
    fn sb(&self) -> u32 {
        assert!(self.fan.is_power_of_two() && self.fan >= 2);
        self.fan.trailing_zeros()
    }

    /// Canonical (uncompressed) encoding; this is what the node hash covers.
    pub fn enc_raw(&self, l: &[Ent], o: &mut Vec<u8>) {
        assert_eq!(l.len(), self.fan);
        match self.kind {
            Kind::Rows => rows_enc(self.t, l, o),
            Kind::Runs => rb_enc(self.t, self.sb(), l, o),
        }
    }
    pub fn dec_raw(&self, b: &[u8], o: &mut [Ent]) {
        match self.kind {
            Kind::Rows => rows_dec(self.t, self.fan, b, o),
            Kind::Runs => rb_dec(self.t, self.sb(), self.fan, b, o),
        }
    }
    #[inline]
    pub fn lk(&self, b: &[u8], i: usize) -> Lk {
        match self.kind {
            Kind::Rows => rows_lk(self.t, b, i),
            Kind::Runs => rb_lk(self.t, self.sb(), b, i),
        }
    }
}

pub fn wrap_enc(w: Wrap, raw: &[u8]) -> Vec<u8> {
    match w {
        Wrap::None => raw.to_vec(),
        Wrap::Lz4 => lz4_flex::block::compress_prepend_size(raw),
        Wrap::Zstd1 => zstd::bulk::compress(raw, 1).expect("zstd"),
    }
}
pub fn wrap_dec(w: Wrap, stored: &[u8], cap: usize) -> std::io::Result<Vec<u8>> {
    let bad = |e: String| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
    match w {
        Wrap::None => Ok(stored.to_vec()),
        Wrap::Lz4 => {
            lz4_flex::block::decompress_size_prepended(stored).map_err(|e| bad(e.to_string()))
        }
        Wrap::Zstd1 => zstd::bulk::decompress(stored, cap).map_err(|e| bad(e.to_string())),
    }
}

// ---------- bit IO (unchanged from leafenc) ----------
struct BW {
    buf: Vec<u8>,
    acc: u64,
    n: u32,
}
impl BW {
    fn new(buf: Vec<u8>) -> Self {
        BW { buf, acc: 0, n: 0 }
    }
    #[inline]
    fn put(&mut self, v: u64, w: u32) {
        if w == 0 {
            return;
        }
        debug_assert!(w <= 56 && v >> w == 0);
        self.acc |= v << self.n;
        self.n += w;
        while self.n >= 8 {
            self.buf.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    fn align(&mut self) {
        if self.n > 0 {
            self.buf.push(self.acc as u8);
            self.acc = 0;
            self.n = 0;
        }
    }
}
#[inline]
fn rd(b: &[u8], bit: usize, w: u32) -> u64 {
    if w == 0 {
        return 0;
    }
    let by = bit >> 3;
    let sh = (bit & 7) as u32;
    let v = if by + 8 <= b.len() {
        u64::from_le_bytes(b[by..by + 8].try_into().unwrap())
    } else {
        let mut x = [0u8; 8];
        let k = b.len() - by;
        x[..k].copy_from_slice(&b[by..]);
        u64::from_le_bytes(x)
    };
    (v >> sh) & ((1u64 << w) - 1)
}
#[inline]
fn bits(v: u64) -> u32 {
    64 - v.leading_zeros()
}
fn idp_of(id: &[u8], t: usize) -> u64 {
    let mut x = [0u8; 8];
    let k = t.min(8);
    x[..k].copy_from_slice(&id[..k]);
    u64::from_le_bytes(x)
}

// ---------- plain rows: [id t][seg u64][off u32][clen u32] ----------
fn rows_enc(t: usize, l: &[Ent], o: &mut Vec<u8>) {
    let rec = t + 16;
    for e in l {
        if e.hole() {
            o.extend(std::iter::repeat(0u8).take(rec));
            continue;
        }
        o.extend_from_slice(&e.id[..t]);
        o.extend_from_slice(&e.seg.to_le_bytes());
        o.extend_from_slice(&e.off.to_le_bytes());
        o.extend_from_slice(&e.clen.to_le_bytes());
    }
}
fn rows_lk(t: usize, b: &[u8], i: usize) -> Lk {
    let r = &b[i * (t + 16)..];
    Lk {
        seg: u64::from_le_bytes(r[t..t + 8].try_into().unwrap()),
        off: u32::from_le_bytes(r[t + 8..t + 12].try_into().unwrap()),
        clen: u32::from_le_bytes(r[t + 12..t + 16].try_into().unwrap()),
        idp: idp_of(r, t),
    }
}
fn rows_dec(t: usize, fan: usize, b: &[u8], o: &mut [Ent]) {
    for i in 0..fan {
        let l = rows_lk(t, b, i);
        let r = &b[i * (t + 16)..];
        let d = &mut o[i];
        *d = Ent::default();
        d.id[..t].copy_from_slice(&r[..t]);
        d.seg = l.seg;
        d.off = l.off;
        d.clen = l.clen;
    }
}

// ---------- runs + bit-packed columns ----------
// Layout: 19-byte header (nruns u16, ndata u16, ndict u16, wseg, wsd, woff, wc,
// wcum u8, base u64), segment dictionary (wsd bits each), run table
// (start:sb | seg_idx:wseg | start_off:woff | rank:sb+1), clen column (wc bits
// per non-hole entry), restart sums every 16 data entries (wcum bits), then the
// id prefixes of non-hole entries (t bytes each).
const RB_HDR: usize = 19;
struct Hd {
    nruns: usize,
    wseg: u32,
    wsd: u32,
    woff: u32,
    wc: u32,
    wcum: u32,
    base: u64,
    dict_o: usize,
    runs_o: usize,
    clen_o: usize,
    cum_o: usize,
    ids_o: usize,
    rw: u32,
    sb: u32,
}
#[allow(clippy::too_many_arguments)]
fn rb_layout(
    sb: u32,
    nruns: usize,
    ndata: usize,
    ndict: usize,
    wseg: u32,
    wsd: u32,
    woff: u32,
    wc: u32,
    wcum: u32,
    base: u64,
) -> Hd {
    let dict_o = RB_HDR;
    let runs_o = dict_o + (ndict * wsd as usize).div_ceil(8);
    let rw = sb + wseg + woff + sb + 1;
    let clen_o = runs_o + (nruns * rw as usize).div_ceil(8);
    let cum_o = clen_o + (ndata * wc as usize).div_ceil(8);
    let cum_len = (ndata.div_ceil(16) * wcum as usize).div_ceil(8);
    Hd {
        nruns,
        wseg,
        wsd,
        woff,
        wc,
        wcum,
        base,
        dict_o,
        runs_o,
        clen_o,
        cum_o,
        ids_o: cum_o + cum_len,
        rw,
        sb,
    }
}
fn rb_parse(sb: u32, b: &[u8]) -> Hd {
    let g16 = |p: usize| u16::from_le_bytes([b[p], b[p + 1]]) as usize;
    rb_layout(
        sb,
        g16(0),
        g16(2),
        g16(4),
        b[6] as u32,
        b[7] as u32,
        b[8] as u32,
        b[9] as u32,
        b[10] as u32,
        u64::from_le_bytes(b[11..19].try_into().unwrap()),
    )
}
fn s_of(b: &[u8], h: &Hd, k: usize) -> u64 {
    let g = k / 16;
    let mut s = if h.wcum == 0 && g == 0 {
        0
    } else {
        rd(b, h.cum_o * 8 + g * h.wcum as usize, h.wcum)
    };
    for j in g * 16..k {
        s += HDR as u64 + rd(b, h.clen_o * 8 + j * h.wc as usize, h.wc);
    }
    s
}
fn rb_enc(t: usize, sb: u32, l: &[Ent], o: &mut Vec<u8>) {
    // runs: (start, seg, off, rank0)
    let mut runs: Vec<(usize, u64, u32, usize)> = Vec::with_capacity(64);
    let mut nd = 0usize;
    let mut prev: Option<&Ent> = None;
    for (i, e) in l.iter().enumerate() {
        let cont = match prev {
            Some(p) if e.hole() && p.hole() => true,
            Some(p) if !e.hole() && !p.hole() && e.seg == p.seg => e.off == p.off + HDR + p.clen,
            _ => false,
        };
        if !cont {
            runs.push((i, e.seg, e.off, nd));
        }
        if !e.hole() {
            nd += 1;
        }
        prev = Some(e);
    }
    let mut dict: Vec<u64> = runs.iter().map(|r| r.1).filter(|&s| s != 0).collect();
    dict.sort_unstable();
    dict.dedup();
    let base = dict.first().copied().unwrap_or(0);
    let wsd = bits(dict.last().copied().unwrap_or(0) - base);
    let wseg = bits(dict.len() as u64);
    let woff = bits(runs.iter().map(|r| r.2 as u64).max().unwrap_or(0));
    let clens: Vec<u32> = l.iter().filter(|e| !e.hole()).map(|e| e.clen).collect();
    let wc = bits(clens.iter().copied().max().unwrap_or(0) as u64);
    let mut cum = Vec::new();
    let mut s = 0u64;
    for (j, c) in clens.iter().enumerate() {
        if j % 16 == 0 {
            cum.push(s);
        }
        s += (HDR + c) as u64;
    }
    let wcum = bits(cum.iter().copied().max().unwrap_or(0));
    let mut w = BW::new(std::mem::take(o));
    w.put(runs.len() as u64, 16);
    w.put(nd as u64, 16);
    w.put(dict.len() as u64, 16);
    for x in [wseg, wsd, woff, wc, wcum] {
        w.put(x as u64, 8);
    }
    w.put(base & 0xffff_ffff, 32);
    w.put(base >> 32, 32);
    for &d in &dict {
        w.put(d - base, wsd);
    }
    w.align();
    for r in &runs {
        let si = if r.1 == 0 {
            0
        } else {
            dict.binary_search(&r.1).unwrap() as u64 + 1
        };
        w.put(r.0 as u64, sb);
        w.put(si, wseg);
        w.put(if r.1 == 0 { 0 } else { r.2 as u64 }, woff);
        w.put(r.3 as u64, sb + 1);
    }
    w.align();
    for &c in &clens {
        w.put(c as u64, wc);
    }
    w.align();
    for &c in &cum {
        w.put(c, wcum);
    }
    w.align();
    let mut o2 = w.buf;
    for e in l.iter().filter(|e| !e.hole()) {
        o2.extend_from_slice(&e.id[..t]);
    }
    *o = o2;
}
fn rb_dec(t: usize, sb: u32, fan: usize, b: &[u8], o: &mut [Ent]) {
    let h = rb_parse(sb, b);
    let mut k = 0usize;
    for r in 0..h.nruns {
        let rec = h.runs_o * 8 + r * h.rw as usize;
        let st = rd(b, rec, h.sb) as usize;
        let en = if r + 1 < h.nruns {
            rd(b, rec + h.rw as usize, h.sb) as usize
        } else {
            fan
        };
        let si = rd(b, rec + h.sb as usize, h.wseg);
        if si == 0 {
            for d in &mut o[st..en] {
                *d = Ent::default();
            }
            continue;
        }
        let a = rd(b, rec + (h.sb + h.wseg) as usize, h.woff) as u32;
        let seg = h.base + rd(b, h.dict_o * 8 + (si as usize - 1) * h.wsd as usize, h.wsd);
        let mut off = a;
        for d in &mut o[st..en] {
            *d = Ent::default();
            d.seg = seg;
            d.id[..t].copy_from_slice(&b[h.ids_o + k * t..h.ids_o + k * t + t]);
            let c = rd(b, h.clen_o * 8 + k * h.wc as usize, h.wc) as u32;
            d.off = off;
            d.clen = c;
            off += HDR + c;
            k += 1;
        }
    }
}
fn rb_lk(t: usize, sb: u32, b: &[u8], i: usize) -> Lk {
    let h = rb_parse(sb, b);
    let (mut lo, mut hi) = (0usize, h.nruns);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if rd(b, h.runs_o * 8 + mid * h.rw as usize, h.sb) as usize <= i {
            lo = mid
        } else {
            hi = mid
        }
    }
    let rec = h.runs_o * 8 + lo * h.rw as usize;
    let st = rd(b, rec, h.sb) as usize;
    let si = rd(b, rec + h.sb as usize, h.wseg);
    if si == 0 {
        return Lk::default();
    }
    let a = rd(b, rec + (h.sb + h.wseg) as usize, h.woff) as u32;
    let rank0 = rd(b, rec + (h.sb + h.wseg + h.woff) as usize, h.sb + 1) as usize;
    let k = rank0 + (i - st);
    let seg = h.base + rd(b, h.dict_o * 8 + (si as usize - 1) * h.wsd as usize, h.wsd);
    let idp = if t > 0 {
        idp_of(&b[h.ids_o + k * t..], t)
    } else {
        0
    };
    let clen = rd(b, h.clen_o * 8 + k * h.wc as usize, h.wc) as u32;
    let off = a + (s_of(b, &h, k) - s_of(b, &h, rank0)) as u32;
    Lk {
        seg,
        off,
        clen,
        idp,
    }
}
