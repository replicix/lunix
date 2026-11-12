//! A point-lookup reader for canonical `rb16` leaves (on-disk-formats.md
//! section 8.3), for the encoded-leaf cache.
//!
//! `lunix_format::node::LeafView` is the reference reader. This one reads the
//! same bytes with the same checks and the same answers, but fetches each
//! bit field with one unaligned 64-bit load and looks up only what the entry
//! needs; at fanout 512 that is about 90 ns against about 290 ns, which is
//! the difference between meeting the 250 ns gate for a lookup in an encoded
//! leaf and missing it (see the milestone report). The format crate owns the
//! layout: [`tests`] below compare every answer with `LeafView` and `Leaf`
//! on random leaves and on damaged bytes, so a layout change breaks the build
//! of this module's tests, not a volume. Callers parse a buffer with
//! `LeafView::parse` once when it is loaded; this reader repeats the size
//! checks, never panics and never reads outside the buffer.

use lunix_format::node::{DataEntry, Entry, LeafParams};
use lunix_format::{IdLen, IdPrefix, LineageIdx, ObjId};
use lunix_iface::{Error, Result};

fn bad(detail: &'static str) -> Error {
    Error::Corrupt {
        what: "map leaf",
        detail,
    }
}

const HEADER_LEN: usize = 22;

fn mask(w: u32) -> u64 {
    if w >= 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    }
}

/// `w` bits starting at bit `bit`, LSB first.
#[inline]
fn bits(buf: &[u8], bit: usize, w: u32) -> Result<u64> {
    if w == 0 {
        return Ok(0);
    }
    let by = bit >> 3;
    let sh = (bit & 7) as u32;
    if sh + w <= 64 {
        if let Some(word) = buf.get(by..).and_then(|s| s.first_chunk::<8>()) {
            return Ok((u64::from_le_bytes(*word) >> sh) & mask(w));
        }
    }
    let n = ((sh + w) as usize).div_ceil(8);
    let s = by
        .checked_add(n)
        .and_then(|end| buf.get(by..end))
        .ok_or(bad("bit field outside the leaf"))?;
    let mut acc: u128 = 0;
    for (i, b) in s.iter().enumerate() {
        acc |= u128::from(*b) << (8 * i);
    }
    Ok(((acc >> sh) as u64) & mask(w))
}

fn bit_width(v: u64) -> u32 {
    64 - v.leading_zeros()
}

/// A parsed leaf header over borrowed bytes.
pub(crate) struct FastLeaf<'a> {
    buf: &'a [u8],
    sb: u32,
    fanout: usize,
    ovh: u64,
    id_len: usize,
    id_kind: IdLen,
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

impl<'a> FastLeaf<'a> {
    pub(crate) fn parse(buf: &'a [u8], p: &LeafParams) -> Result<Self> {
        let sb = match p.fanout {
            256 => 8,
            512 => 9,
            _ => return Err(bad("unsupported fanout")),
        };
        let h = buf.get(..HEADER_LEN).ok_or(bad("short leaf"))?;
        let u16at = |o: usize| usize::from(u16::from_le_bytes([h[o], h[o + 1]]));
        if h[0] != 0 || u16at(1) != usize::from(p.fanout) {
            return Err(bad("not a leaf of this fanout"));
        }
        let fanout = usize::from(p.fanout);
        let (nruns, ndata, ndict) = (u16at(3), u16at(5), u16at(7));
        let (wseg, wsd, woff, wc, wcum) = (
            u32::from(h[9]),
            u32::from(h[10]),
            u32::from(h[11]),
            u32::from(h[12]),
            u32::from(h[13]),
        );
        let mut b8 = [0u8; 8];
        b8.copy_from_slice(&h[14..22]);
        let base = u64::from_le_bytes(b8);
        if nruns == 0 || nruns > fanout || ndata > fanout || ndict > ndata {
            return Err(bad("count out of range"));
        }
        if wseg != bit_width(ndict as u64) || wsd > 64 || woff > 32 || wc > 32 || wcum > 48 {
            return Err(bad("column width out of range"));
        }
        let rw = (sb + wseg + woff + sb + 1) as usize;
        let id_len = p.id_len.len();
        let dict_o = HEADER_LEN;
        let lin_o = dict_o + (ndict * wsd as usize).div_ceil(8);
        let runs_o = lin_o + ndict;
        let clen_o = runs_o + (nruns * rw).div_ceil(8);
        let cum_o = clen_o + (ndata * wc as usize).div_ceil(8);
        let ids_o = cum_o + (ndata.div_ceil(16) * wcum as usize).div_ceil(8);
        if buf.len() != ids_o + ndata * id_len {
            return Err(bad("leaf size does not match its columns"));
        }
        Ok(Self {
            buf,
            sb,
            fanout,
            ovh: u64::from(p.ovh),
            id_len,
            id_kind: p.id_len,
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

    #[inline]
    fn run_start(&self, r: usize) -> Result<usize> {
        Ok(bits(self.buf, self.runs_o * 8 + r * self.rw, self.sb)? as usize)
    }

    #[inline]
    fn clen(&self, k: usize) -> Result<u64> {
        bits(self.buf, self.clen_o * 8 + k * self.wc as usize, self.wc)
    }

    /// Bytes of extents `from..to` (indexes among the non-hole entries).
    fn extent_bytes(&self, from: usize, to: usize) -> Result<u64> {
        let mut s = 0u64;
        for j in from..to {
            s = s
                .checked_add(self.ovh + self.clen(j)?)
                .ok_or(bad("offset overflow"))?;
        }
        Ok(s)
    }

    /// `S(k)`: bytes of the first `k` non-hole extents.
    fn s_of(&self, k: usize) -> Result<u64> {
        let g = k / 16;
        let cum = bits(self.buf, self.cum_o * 8 + g * self.wcum as usize, self.wcum)?;
        cum.checked_add(self.extent_bytes(g * 16, k)?)
            .ok_or(bad("offset overflow"))
    }

    /// Entry `i` (0 ..= fanout - 1).
    pub(crate) fn get(&self, i: usize) -> Result<Entry> {
        if i >= self.fanout {
            return Err(bad("entry index past the leaf"));
        }
        let (mut lo, mut hi) = (0usize, self.nruns);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if self.run_start(mid)? <= i {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let at = self.runs_o * 8 + lo * self.rw;
        let start = self.run_start(lo)?;
        if start > i {
            return Err(bad("first run does not start at 0"));
        }
        let sb = self.sb as usize;
        let si = bits(self.buf, at + sb, self.wseg)? as usize;
        if si == 0 {
            return Ok(Entry::Hole);
        }
        let start_off = bits(self.buf, at + sb + self.wseg as usize, self.woff)?;
        let rank = bits(
            self.buf,
            at + sb + (self.wseg + self.woff) as usize,
            self.sb + 1,
        )? as usize;
        if si > self.ndict {
            return Err(bad("segment index past the dictionary"));
        }
        let delta = bits(
            self.buf,
            self.dict_o * 8 + (si - 1) * self.wsd as usize,
            self.wsd,
        )?;
        let seg = self
            .base
            .checked_add(delta)
            .ok_or(bad("segment overflow"))?;
        let lineage = *self
            .buf
            .get(self.lin_o + si - 1)
            .ok_or(bad("lineage column outside the leaf"))?;
        let k = rank
            .checked_add(i - start)
            .filter(|&k| k < self.ndata)
            .ok_or(bad("data index past ndata"))?;
        let clen = u32::try_from(self.clen(k)?).map_err(|_| bad("clen overflow"))?;
        let within = if k / 16 == rank / 16 {
            self.extent_bytes(rank, k)?
        } else {
            self.s_of(k)?
                .checked_sub(self.s_of(rank)?)
                .ok_or(bad("cum column not monotone"))?
        };
        let off = start_off
            .checked_add(within)
            .and_then(|o| u32::try_from(o).ok())
            .ok_or(bad("extent offset overflow"))?;
        let from = self.ids_o + k * self.id_len;
        let id = self
            .buf
            .get(from..from + self.id_len)
            .ok_or(bad("id column outside the leaf"))?;
        Ok(Entry::Data(DataEntry {
            lineage: LineageIdx(lineage),
            seg: ObjId::from_raw(seg)?,
            off,
            clen,
            prefix: IdPrefix::from_slice(self.id_kind, id)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunix_format::node::{Leaf, LeafView};
    use lunix_format::ChunkId;
    use proptest::prelude::*;

    /// A random leaf: runs of holes and of contiguous data in a few segments,
    /// with jumps, as destage and compaction produce them.
    fn leaf_strategy(fanout: u16, ovh: u32, id_len: IdLen) -> impl Strategy<Value = Leaf> {
        let runs = proptest::collection::vec(
            (
                any::<bool>(),
                1usize..40,
                0u64..6,
                any::<u32>(),
                1u32..70_000,
            ),
            1..40,
        );
        (runs, any::<u64>()).prop_map(move |(runs, salt)| {
            let mut entries = Vec::new();
            let mut n = 0u64;
            for (hole, len, seg, off0, clen0) in runs {
                for j in 0..len as u64 {
                    if entries.len() >= usize::from(fanout) {
                        break;
                    }
                    n += 1;
                    if hole {
                        entries.push(Entry::Hole);
                        continue;
                    }
                    let clen = 1 + (clen0.wrapping_mul(j as u32 + 1) % 65_536);
                    let mut id = [0u8; 32];
                    id[..8].copy_from_slice(&(salt ^ n).to_le_bytes());
                    id[8..16].copy_from_slice(&n.to_le_bytes());
                    let off = u64::from(off0 >> 8) + j * (u64::from(ovh) + 40_000);
                    entries.push(Entry::Data(DataEntry {
                        lineage: LineageIdx(0),
                        seg: ObjId::new(0, 1 + seg * 3).expect("seg"),
                        off: u32::try_from(off).expect("small"),
                        clen,
                        prefix: IdPrefix::of(&ChunkId(id), id_len),
                    }));
                }
            }
            entries.resize(usize::from(fanout), Entry::Hole);
            Leaf { entries }
        })
    }

    fn params(fanout: u16, ovh: u32, id_len: IdLen) -> LeafParams {
        LeafParams {
            fanout,
            id_len,
            ovh,
        }
    }

    fn check(leaf: &Leaf, p: &LeafParams) -> std::result::Result<(), TestCaseError> {
        let mut buf = Vec::new();
        leaf.encode(p, &mut buf).expect("encode");
        let fast = FastLeaf::parse(&buf, p).expect("parse");
        let view = LeafView::parse(&buf, p).expect("view");
        for (i, want) in leaf.entries.iter().enumerate() {
            prop_assert_eq!(fast.get(i).expect("fast"), *want, "entry {}", i);
            prop_assert_eq!(view.get(i).expect("view"), *want);
        }
        prop_assert!(fast.get(leaf.entries.len()).is_err());
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        #[test]
        fn agrees_with_the_reference_on_valid_leaves(
            leaf512 in leaf_strategy(512, 68, IdLen::Sixteen),
            leaf256 in leaf_strategy(256, 108, IdLen::ThirtyTwo),
            leaf_lean in leaf_strategy(512, 68, IdLen::Zero),
        ) {
            check(&leaf512, &params(512, 68, IdLen::Sixteen))?;
            check(&leaf256, &params(256, 108, IdLen::ThirtyTwo))?;
            check(&leaf_lean, &params(512, 68, IdLen::Zero))?;
        }

        #[test]
        fn damaged_bytes_never_panic_and_never_disagree_with_the_reference(
            leaf in leaf_strategy(512, 68, IdLen::Sixteen),
            flips in proptest::collection::vec((any::<usize>(), 1u8..=255), 1..4),
            cut in proptest::option::of(any::<usize>()),
        ) {
            let p = params(512, 68, IdLen::Sixteen);
            let mut buf = Vec::new();
            leaf.encode(&p, &mut buf).expect("encode");
            for (at, x) in flips {
                let n = buf.len();
                buf[at % n] ^= x;
            }
            if let Some(c) = cut {
                let n = buf.len();
                buf.truncate(c % n);
            }
            let (fast, view) = (FastLeaf::parse(&buf, &p), LeafView::parse(&buf, &p));
            prop_assert_eq!(fast.is_ok(), view.is_ok());
            if let (Ok(fast), Ok(view)) = (fast, view) {
                for i in 0..512 {
                    match (fast.get(i), view.get(i)) {
                        (Ok(a), Ok(b)) => prop_assert_eq!(a, b, "entry {}", i),
                        (Err(_), Err(_)) => {}
                        (a, b) => prop_assert!(false, "entry {}: {:?} against {:?}", i, a.is_ok(), b.is_ok()),
                    }
                }
            }
        }
    }
}
