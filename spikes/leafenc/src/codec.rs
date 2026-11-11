//! Leaf encodings. Each `Raw` codec encodes 512 entries; `Variant` optionally wraps with zstd / lz4.
use crate::gen::*;
use std::cell::RefCell;

#[derive(Clone, Copy, Default)]
pub struct DE {
    pub id: [u8; 32], // first `t` bytes valid
    pub seg: u64,
    pub off: u32,
    pub clen: u32,
    pub slot: u16,
}
#[derive(Clone, Copy, Default, Debug)]
pub struct Lk {
    pub seg: u64,
    pub off: u32,
    pub clen: u32,
    pub slot: u16,
    pub idp: u64, // first min(t,8) id bytes, LE
}

pub trait Raw {
    fn name(&self) -> String;
    fn t(&self) -> usize;
    fn slot(&self) -> bool;
    fn enc(&self, l: &[E], o: &mut Vec<u8>);
    fn dec(&self, b: &[u8], o: &mut [DE]);
    fn lk(&self, b: &[u8], i: usize) -> Lk;
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
fn idp_buf(b: &[u8], t: usize) -> u64 {
    idp_of(b, t)
}

// ---------- bit IO ----------
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
        debug_assert!(w <= 56 && (w == 64 || v >> w == 0));
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

fn put_vi(o: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        o.push((v as u8) | 0x80);
        v >>= 7;
    }
    o.push(v as u8);
}
#[inline]
fn get_vi(b: &[u8], p: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut s = 0;
    loop {
        let c = b[*p];
        *p += 1;
        v |= ((c & 0x7f) as u64) << s;
        if c & 0x80 == 0 {
            return v;
        }
        s += 7;
    }
}
fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}
fn uzz(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

// ---------- row layout: [id t][seg u64][off u32][clen u32]  or  [id t][seg u64][slot u16] ----------
pub struct Row {
    pub t: usize,
    pub slot: bool,
}
impl Row {
    fn rec(&self) -> usize {
        self.t + if self.slot { 10 } else { 16 }
    }
}
impl Raw for Row {
    fn name(&self) -> String {
        format!("row{}{}", if self.slot { "-slot" } else { "" }, self.t)
    }
    fn t(&self) -> usize {
        self.t
    }
    fn slot(&self) -> bool {
        self.slot
    }
    fn enc(&self, l: &[E], o: &mut Vec<u8>) {
        for e in l {
            if e.hole() {
                o.extend(std::iter::repeat(0u8).take(self.rec()));
                continue;
            }
            o.extend_from_slice(&idbytes(e.idseed)[..self.t]);
            o.extend_from_slice(&e.seg.to_le_bytes());
            if self.slot {
                o.extend_from_slice(&e.slot.to_le_bytes());
            } else {
                o.extend_from_slice(&e.off.to_le_bytes());
                o.extend_from_slice(&e.clen.to_le_bytes());
            }
        }
    }
    fn dec(&self, b: &[u8], o: &mut [DE]) {
        for i in 0..FAN {
            let l = self.lk(b, i);
            let r = &b[i * self.rec()..];
            let d = &mut o[i];
            d.id[..self.t].copy_from_slice(&r[..self.t]);
            d.seg = l.seg;
            d.off = l.off;
            d.clen = l.clen;
            d.slot = l.slot;
        }
    }
    fn lk(&self, b: &[u8], i: usize) -> Lk {
        let r = &b[i * self.rec()..];
        let t = self.t;
        let seg = u64::from_le_bytes(r[t..t + 8].try_into().unwrap());
        let mut l = Lk { seg, idp: idp_buf(r, t), ..Default::default() };
        if self.slot {
            l.slot = u16::from_le_bytes(r[t + 8..t + 10].try_into().unwrap());
        } else {
            l.off = u32::from_le_bytes(r[t + 8..t + 12].try_into().unwrap());
            l.clen = u32::from_le_bytes(r[t + 12..t + 16].try_into().unwrap());
        }
        l
    }
}

// ---------- column layout (struct of arrays); delta variant makes seg/off deltas (needs scan) ----------
pub struct Col {
    pub t: usize,
    pub delta: bool,
}
impl Raw for Col {
    fn name(&self) -> String {
        format!("col{}{}", if self.delta { "-delta" } else { "" }, self.t)
    }
    fn t(&self) -> usize {
        self.t
    }
    fn slot(&self) -> bool {
        false
    }
    fn enc(&self, l: &[E], o: &mut Vec<u8>) {
        for e in l {
            let id = idbytes(e.idseed);
            if e.hole() {
                o.extend(std::iter::repeat(0u8).take(self.t));
            } else {
                o.extend_from_slice(&id[..self.t]);
            }
        }
        let mut prev = E::default();
        for e in l {
            let v = if self.delta { zz(e.seg as i64 - prev.seg as i64) } else { e.seg };
            o.extend_from_slice(&v.to_le_bytes());
            prev = *e;
        }
        prev = E::default();
        for e in l {
            let v = if self.delta {
                let exp = prev.off.wrapping_add(HDR).wrapping_add(prev.clen);
                zz(e.off.wrapping_sub(exp) as i32 as i64) as u32
            } else {
                e.off
            };
            o.extend_from_slice(&v.to_le_bytes());
            prev = *e;
        }
        for e in l {
            o.extend_from_slice(&e.clen.to_le_bytes());
        }
    }
    fn dec(&self, b: &[u8], o: &mut [DE]) {
        let t = self.t;
        let (so, oo, co) = (FAN * t, FAN * t + FAN * 8, FAN * t + FAN * 12);
        let (mut ps, mut po, mut pc) = (0u64, 0u32, 0u32);
        for i in 0..FAN {
            let s = u64::from_le_bytes(b[so + i * 8..so + i * 8 + 8].try_into().unwrap());
            let f = u32::from_le_bytes(b[oo + i * 4..oo + i * 4 + 4].try_into().unwrap());
            let c = u32::from_le_bytes(b[co + i * 4..co + i * 4 + 4].try_into().unwrap());
            let (seg, off) = if self.delta {
                (
                    (ps as i64 + uzz(s)) as u64,
                    po.wrapping_add(HDR).wrapping_add(pc).wrapping_add(uzz(f as u64) as i32 as u32),
                )
            } else {
                (s, f)
            };
            let d = &mut o[i];
            d.id[..t].copy_from_slice(&b[i * t..i * t + t]);
            d.seg = seg;
            d.off = off;
            d.clen = c;
            ps = seg;
            po = off;
            pc = c;
        }
    }
    fn lk(&self, b: &[u8], i: usize) -> Lk {
        let t = self.t;
        let (so, oo, co) = (FAN * t, FAN * t + FAN * 8, FAN * t + FAN * 12);
        let gs = |k: usize| u64::from_le_bytes(b[so + k * 8..so + k * 8 + 8].try_into().unwrap());
        let go = |k: usize| u32::from_le_bytes(b[oo + k * 4..oo + k * 4 + 4].try_into().unwrap());
        let gc = |k: usize| u32::from_le_bytes(b[co + k * 4..co + k * 4 + 4].try_into().unwrap());
        let idp = idp_buf(&b[i * t..], t);
        if !self.delta {
            return Lk { seg: gs(i), off: go(i), clen: gc(i), slot: 0, idp };
        }
        let (mut ps, mut po, mut pc) = (0u64, 0u32, 0u32);
        for k in 0..=i {
            ps = (ps as i64 + uzz(gs(k))) as u64;
            po = po.wrapping_add(HDR).wrapping_add(pc).wrapping_add(uzz(go(k) as u64) as i32 as u32);
            pc = gc(k);
        }
        Lk { seg: ps, off: po, clen: pc, slot: 0, idp }
    }
}

// ---------- runs + bitpacked columns ----------
pub struct Rb {
    pub t: usize,
    pub slot: bool,
}
struct Hd {
    nruns: usize,
    ndata: usize,
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
}
const RB_HDR: usize = 19;
fn rb_layout(nruns: usize, ndata: usize, ndict: usize, wseg: u32, wsd: u32, woff: u32, wc: u32, wcum: u32, base: u64, slot: bool) -> Hd {
    let dict_o = RB_HDR;
    let runs_o = dict_o + (ndict * wsd as usize + 7) / 8;
    let rw = 9 + wseg + woff + 10;
    let clen_o = runs_o + (nruns * rw as usize + 7) / 8;
    let clen_len = if slot { 0 } else { (ndata * wc as usize + 7) / 8 };
    let cum_o = clen_o + clen_len;
    let cum_len = if slot { 0 } else { ((ndata + 15) / 16 * wcum as usize + 7) / 8 };
    Hd { nruns, ndata, wseg, wsd, woff, wc, wcum, base, dict_o, runs_o, clen_o, cum_o, ids_o: cum_o + cum_len, rw }
}
fn rb_parse(b: &[u8], slot: bool) -> Hd {
    let g16 = |p: usize| u16::from_le_bytes([b[p], b[p + 1]]) as usize;
    rb_layout(
        g16(0),
        g16(2),
        g16(4),
        b[6] as u32,
        b[7] as u32,
        b[8] as u32,
        b[9] as u32,
        b[10] as u32,
        u64::from_le_bytes(b[11..19].try_into().unwrap()),
        slot,
    )
}
impl Rb {
    fn s_of(&self, b: &[u8], h: &Hd, k: usize) -> u64 {
        // sum of (HDR+clen) over data entries [0,k)
        let g = k / 16;
        let mut s = if h.wcum == 0 && g == 0 { 0 } else { rd(b, h.cum_o * 8 + g * h.wcum as usize, h.wcum) };
        for j in g * 16..k {
            s += HDR as u64 + rd(b, h.clen_o * 8 + j * h.wc as usize, h.wc);
        }
        s
    }
}
impl Raw for Rb {
    fn name(&self) -> String {
        format!("runs-bp{}{}", if self.slot { "-slot" } else { "" }, self.t)
    }
    fn t(&self) -> usize {
        self.t
    }
    fn slot(&self) -> bool {
        self.slot
    }
    fn enc(&self, l: &[E], o: &mut Vec<u8>) {
        // runs: (start, seg, a(off|slot), rank0)
        let mut runs: Vec<(usize, u64, u32, usize)> = Vec::with_capacity(64);
        let mut nd = 0usize;
        let mut prev: Option<&E> = None;
        for (i, e) in l.iter().enumerate() {
            let cont = match prev {
                Some(p) if e.hole() && p.hole() => true,
                Some(p) if !e.hole() && !p.hole() && e.seg == p.seg => {
                    if self.slot {
                        e.slot == p.slot.wrapping_add(1)
                    } else {
                        e.off == p.off + HDR + p.clen
                    }
                }
                _ => false,
            };
            if !cont {
                runs.push((i, e.seg, if self.slot { e.slot as u32 } else { e.off }, nd));
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
        w.put(base, 32);
        w.put(base >> 32, 32);
        for &d in &dict {
            w.put(d - base, wsd);
        }
        w.align();
        for r in &runs {
            let si = if r.1 == 0 { 0 } else { dict.binary_search(&r.1).unwrap() as u64 + 1 };
            w.put(r.0 as u64, 9);
            w.put(si, wseg);
            w.put(if r.1 == 0 { 0 } else { r.2 as u64 }, woff);
            w.put(r.3 as u64, 10);
        }
        w.align();
        if !self.slot {
            for &c in &clens {
                w.put(c as u64, wc);
            }
            w.align();
            for &c in &cum {
                w.put(c, wcum);
            }
            w.align();
        }
        let mut o2 = w.buf;
        for e in l.iter().filter(|e| !e.hole()) {
            o2.extend_from_slice(&idbytes(e.idseed)[..self.t]);
        }
        *o = o2;
    }
    fn dec(&self, b: &[u8], o: &mut [DE]) {
        let h = rb_parse(b, self.slot);
        let t = self.t;
        let mut k = 0usize; // data rank
        for r in 0..h.nruns {
            let rec = h.runs_o * 8 + r * h.rw as usize;
            let st = rd(b, rec, 9) as usize;
            let en = if r + 1 < h.nruns { rd(b, rec + h.rw as usize, 9) as usize } else { FAN };
            let si = rd(b, rec + 9, h.wseg);
            if si == 0 {
                for d in &mut o[st..en] {
                    *d = DE::default();
                }
                continue;
            }
            let a = rd(b, rec + 9 + h.wseg as usize, h.woff) as u32;
            let seg = h.base + rd(b, h.dict_o * 8 + (si as usize - 1) * h.wsd as usize, h.wsd);
            let mut off = a;
            for i in st..en {
                let d = &mut o[i];
                d.seg = seg;
                d.id[..t].copy_from_slice(&b[h.ids_o + k * t..h.ids_o + k * t + t]);
                if self.slot {
                    d.slot = (a + (i - st) as u32) as u16;
                    d.off = 0;
                    d.clen = 0;
                } else {
                    let c = rd(b, h.clen_o * 8 + k * h.wc as usize, h.wc) as u32;
                    d.off = off;
                    d.clen = c;
                    off += HDR + c;
                }
                k += 1;
            }
        }
    }
    fn lk(&self, b: &[u8], i: usize) -> Lk {
        let h = rb_parse(b, self.slot);
        let (mut lo, mut hi) = (0usize, h.nruns);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if rd(b, h.runs_o * 8 + mid * h.rw as usize, 9) as usize <= i {
                lo = mid
            } else {
                hi = mid
            }
        }
        let rec = h.runs_o * 8 + lo * h.rw as usize;
        let st = rd(b, rec, 9) as usize;
        let si = rd(b, rec + 9, h.wseg);
        if si == 0 {
            return Lk::default();
        }
        let a = rd(b, rec + 9 + h.wseg as usize, h.woff) as u32;
        let rank0 = rd(b, rec + 9 + (h.wseg + h.woff) as usize, 10) as usize;
        let k = rank0 + (i - st);
        let seg = h.base + rd(b, h.dict_o * 8 + (si as usize - 1) * h.wsd as usize, h.wsd);
        let idp = if self.t > 0 { idp_buf(&b[h.ids_o + k * self.t..], self.t) } else { 0 };
        let _ = h.ndata;
        if self.slot {
            Lk { seg, slot: (a + (i - st) as u32) as u16, idp, ..Default::default() }
        } else {
            let clen = rd(b, h.clen_o * 8 + k * h.wc as usize, h.wc) as u32;
            let off = a + (self.s_of(b, &h, k) - self.s_of(b, &h, rank0)) as u32;
            Lk { seg, off, clen, slot: 0, idp }
        }
    }
}

// ---------- runs + varint streams (lookup must scan) ----------
pub struct Rv {
    pub t: usize,
    pub slot: bool,
}
impl Raw for Rv {
    fn name(&self) -> String {
        format!("runs-vi{}{}", if self.slot { "-slot" } else { "" }, self.t)
    }
    fn t(&self) -> usize {
        self.t
    }
    fn slot(&self) -> bool {
        self.slot
    }
    fn enc(&self, l: &[E], o: &mut Vec<u8>) {
        let mut runs: Vec<(usize, usize, bool)> = Vec::new(); // start,len,hole
        for (i, e) in l.iter().enumerate() {
            let cont = if i == 0 {
                false
            } else {
                let p = &l[i - 1];
                if e.hole() && p.hole() {
                    true
                } else if !e.hole() && !p.hole() && e.seg == p.seg {
                    if self.slot {
                        e.slot == p.slot.wrapping_add(1)
                    } else {
                        e.off == p.off + HDR + p.clen
                    }
                } else {
                    false
                }
            };
            if cont {
                runs.last_mut().unwrap().1 += 1;
            } else {
                runs.push((i, 1, e.hole()));
            }
        }
        let nd = l.iter().filter(|e| !e.hole()).count();
        o.extend_from_slice(&(runs.len() as u16).to_le_bytes());
        o.extend_from_slice(&(nd as u16).to_le_bytes());
        let mut ps = 0u64;
        for r in &runs {
            put_vi(o, ((r.1 as u64) << 1) | r.2 as u64);
            if !r.2 {
                let e = &l[r.0];
                put_vi(o, zz(e.seg as i64 - ps as i64));
                ps = e.seg;
                put_vi(o, if self.slot { e.slot as u64 } else { e.off as u64 });
            }
        }
        if !self.slot {
            for e in l.iter().filter(|e| !e.hole()) {
                put_vi(o, e.clen as u64);
            }
        }
        for e in l.iter().filter(|e| !e.hole()) {
            o.extend_from_slice(&idbytes(e.idseed)[..self.t]);
        }
    }
    fn dec(&self, b: &[u8], o: &mut [DE]) {
        let (nr, nd) = (u16::from_le_bytes([b[0], b[1]]) as usize, u16::from_le_bytes([b[2], b[3]]) as usize);
        let mut p = 4;
        let mut runs = Vec::with_capacity(nr);
        let mut ps = 0u64;
        for _ in 0..nr {
            let x = get_vi(b, &mut p);
            let (len, hole) = ((x >> 1) as usize, x & 1 == 1);
            if hole {
                runs.push((len, true, 0u64, 0u32));
            } else {
                ps = (ps as i64 + uzz(get_vi(b, &mut p))) as u64;
                let a = get_vi(b, &mut p) as u32;
                runs.push((len, false, ps, a));
            }
        }
        let mut cl = Vec::with_capacity(nd);
        if !self.slot {
            for _ in 0..nd {
                cl.push(get_vi(b, &mut p) as u32);
            }
        }
        let ids = p;
        let (mut i, mut k) = (0usize, 0usize);
        for &(len, hole, seg, a) in &runs {
            let mut off = a;
            for j in 0..len {
                let d = &mut o[i];
                if hole {
                    *d = DE::default();
                } else {
                    d.seg = seg;
                    d.id[..self.t].copy_from_slice(&b[ids + k * self.t..ids + k * self.t + self.t]);
                    if self.slot {
                        d.slot = (a + j as u32) as u16;
                    } else {
                        d.off = off;
                        d.clen = cl[k];
                        off += HDR + cl[k];
                    }
                    k += 1;
                }
                i += 1;
            }
        }
    }
    fn lk(&self, b: &[u8], idx: usize) -> Lk {
        let nr = u16::from_le_bytes([b[0], b[1]]) as usize;
        let mut p = 4;
        let (mut i, mut k) = (0usize, 0usize);
        let mut ps = 0u64;
        let mut found: Option<(u64, u32, usize, usize)> = None; // seg, a, kstart, i_in_run
        for _ in 0..nr {
            let x = get_vi(b, &mut p);
            let (len, hole) = ((x >> 1) as usize, x & 1 == 1);
            if hole {
                if idx >= i && idx < i + len {
                    return Lk::default();
                }
            } else {
                ps = (ps as i64 + uzz(get_vi(b, &mut p))) as u64;
                let a = get_vi(b, &mut p) as u32;
                if idx >= i && idx < i + len && found.is_none() {
                    found = Some((ps, a, k, idx - i));
                }
                k += len;
            }
            i += len;
            if found.is_some() {
                // must still skip the remaining run headers to reach the clen stream
            }
        }
        let (seg, a, k0, j) = found.unwrap();
        let kk = k0 + j;
        let idp_at = |ids: usize| if self.t > 0 { idp_buf(&b[ids + kk * self.t..], self.t) } else { 0 };
        if self.slot {
            return Lk { seg, slot: (a + j as u32) as u16, idp: idp_at(p), ..Default::default() };
        }
        let (mut s, mut c) = (0u32, 0u32);
        for q in 0..=kk {
            let v = get_vi(b, &mut p) as u32;
            if q >= k0 && q < kk {
                s += HDR + v;
            }
            if q == kk {
                c = v;
            }
        }
        let nd_rest = k - kk - 1; // remaining clens to skip to reach ids
        for _ in 0..nd_rest {
            get_vi(b, &mut p);
        }
        Lk { seg, off: a + s, clen: c, slot: 0, idp: idp_at(p) }
    }
}

// ---------- wrappers ----------
#[derive(Clone, Copy, PartialEq)]
pub enum Wrap {
    None,
    Z(i32),
    L4,
}
pub struct Variant {
    pub raw: Box<dyn Raw>,
    pub w: Wrap,
    zc: RefCell<Option<zstd::bulk::Compressor<'static>>>,
    zd: RefCell<zstd::bulk::Decompressor<'static>>,
    scratch: RefCell<Vec<u8>>,
    scratch2: RefCell<Vec<u8>>,
}
impl Variant {
    pub fn new(raw: Box<dyn Raw>, w: Wrap) -> Self {
        let zc = if let Wrap::Z(l) = w { Some(zstd::bulk::Compressor::new(l).unwrap()) } else { None };
        Variant {
            raw,
            w,
            zc: RefCell::new(zc),
            zd: RefCell::new(zstd::bulk::Decompressor::new().unwrap()),
            scratch: RefCell::new(vec![0u8; 1 << 17]),
            scratch2: RefCell::new(vec![0u8; 1 << 17]),
        }
    }
    pub fn name(&self) -> String {
        let n = self.raw.name();
        match self.w {
            Wrap::None => n,
            Wrap::Z(l) => format!("{n}+zstd{l}"),
            Wrap::L4 => format!("{n}+lz4"),
        }
    }
    pub fn enc(&self, l: &[E], out: &mut Vec<u8>) {
        out.clear();
        if self.w == Wrap::None {
            return self.raw.enc(l, out);
        }
        let mut s = self.scratch.borrow_mut();
        let mut tmp = std::mem::take(&mut *s);
        tmp.clear();
        self.raw.enc(l, &mut tmp);
        let mut c = self.scratch2.borrow_mut();
        let n = match self.w {
            Wrap::Z(_) => self.zc.borrow_mut().as_mut().unwrap().compress_to_buffer(&tmp, &mut c[..]).unwrap(),
            Wrap::L4 => {
                out.extend_from_slice(&(tmp.len() as u16).to_le_bytes());
                lz4_flex::block::compress_into(&tmp, &mut c[..]).unwrap()
            }
            Wrap::None => unreachable!(),
        };
        out.extend_from_slice(&c[..n]);
        tmp.resize(1 << 17, 0);
        *s = tmp;
    }
    /// run `f` on the raw (decompressed) bytes
    #[inline]
    pub fn with_raw<R>(&self, b: &[u8], f: impl FnOnce(&[u8]) -> R) -> R {
        match self.w {
            Wrap::None => f(b),
            Wrap::Z(_) => {
                let mut c = self.scratch2.borrow_mut();
                let n = self.zd.borrow_mut().decompress_to_buffer(b, &mut c[..]).unwrap();
                f(&c[..n])
            }
            Wrap::L4 => {
                let mut c = self.scratch2.borrow_mut();
                let rl = u16::from_le_bytes([b[0], b[1]]) as usize;
                let n = lz4_flex::block::decompress_into(&b[2..], &mut c[..rl]).unwrap();
                f(&c[..n])
            }
        }
    }
    pub fn dec(&self, b: &[u8], o: &mut [DE]) {
        self.with_raw(b, |r| self.raw.dec(r, o))
    }
    pub fn lk(&self, b: &[u8], i: usize) -> Lk {
        self.with_raw(b, |r| self.raw.lk(r, i))
    }
}

pub fn all_variants() -> Vec<Variant> {
    let mut v = Vec::new();
    let mk = |f: &dyn Fn() -> Box<dyn Raw>, v: &mut Vec<Variant>, ws: &[Wrap]| {
        for &w in ws {
            v.push(Variant::new(f(), w));
        }
    };
    let all = [Wrap::None, Wrap::Z(1), Wrap::Z(3), Wrap::L4];
    let nz = [Wrap::None, Wrap::Z(1)];
    for t in [32, 16, 8, 0] {
        mk(&|| Box::new(Row { t, slot: false }), &mut v, &all);
    }
    for t in [16, 0] {
        mk(&|| Box::new(Row { t, slot: true }), &mut v, &nz);
    }
    for t in [32, 16, 8, 0] {
        mk(&|| Box::new(Col { t, delta: false }), &mut v, &all);
    }
    for t in [16, 0] {
        mk(&|| Box::new(Col { t, delta: true }), &mut v, &nz);
    }
    for t in [32, 16, 8, 0] {
        mk(&|| Box::new(Rb { t, slot: false }), &mut v, &all);
    }
    for t in [16, 0] {
        mk(&|| Box::new(Rb { t, slot: true }), &mut v, &all);
    }
    for t in [16, 0] {
        mk(&|| Box::new(Rv { t, slot: false }), &mut v, &all);
    }
    mk(&|| Box::new(Rv { t: 0, slot: true }), &mut v, &nz);
    v
}
