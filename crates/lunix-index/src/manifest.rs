//! The local manifest `index/manifest` (not a frozen format, plan M6).
//!
//! It names the runs of the index in recency order (oldest first), the
//! condemned segments and the dropped segments. It is replaced atomically and
//! the previous generation is kept as `manifest.prev`, so a manifest or a run
//! that is damaged costs one generation, never the index: `catch_up` restores
//! the entries from segment footers.
//!
//! ```text
//! magic "LXIM" | ver u8 = 1 | reserved [3] | generation u64
//! nruns u32 | nruns x (run u64, size u64, entries u64)
//! ncondemned u32 | ncondemned x seg u64      (ascending)
//! ndropped u32   | ndropped x seg u64        (ascending)
//! crc32c of everything before it (u32)
//! ```

use std::collections::BTreeSet;

use lunix_format::bytes::{crc32c, Put, Reader};
use lunix_format::{ObjId, RunId, SegId};
use lunix_iface::{Error, Result};

const MAGIC: &[u8; 4] = b"LXIM";
const WHAT: &str = "index manifest";
const MAX_RUNS: u32 = 1 << 16;
const MAX_SEGS: u32 = 1 << 24;

/// One run as the manifest names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RunMeta {
    pub id: RunId,
    pub size: u64,
    pub entries: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct Manifest {
    pub generation: u64,
    pub runs: Vec<RunMeta>,
    pub condemned: BTreeSet<SegId>,
    pub dropped: BTreeSet<SegId>,
}

impl Manifest {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(32 + self.runs.len() * 24);
        b.extend_from_slice(MAGIC);
        b.put_u8(1);
        b.put_zeros(3);
        b.put_u64(self.generation);
        b.put_u32(self.runs.len() as u32);
        for r in &self.runs {
            b.put_u64(r.id.raw());
            b.put_u64(r.size);
            b.put_u64(r.entries);
        }
        for set in [&self.condemned, &self.dropped] {
            b.put_u32(set.len() as u32);
            for s in set {
                b.put_u64(s.raw());
            }
        }
        let crc = crc32c(&b);
        b.put_u32(crc);
        b
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<Self> {
        let body = buf.len().checked_sub(4).ok_or(Error::Corrupt {
            what: WHAT,
            detail: "too short",
        })?;
        let mut r = Reader::new(&buf[..body], WHAT);
        let mut crc = Reader::new(&buf[body..], WHAT);
        if crc.u32()? != crc32c(&buf[..body]) {
            return Err(Error::Corrupt {
                what: WHAT,
                detail: "checksum",
            });
        }
        r.magic(MAGIC)?;
        r.version_u8(1)?;
        r.reserved(3)?;
        let generation = r.u64()?;
        let nruns = r.u32()?;
        r.check_count("runs", u64::from(nruns), u64::from(MAX_RUNS), 24)?;
        let mut runs = Vec::with_capacity(nruns as usize);
        for _ in 0..nruns {
            runs.push(RunMeta {
                id: ObjId::from_raw(r.u64()?)?,
                size: r.u64()?,
                entries: r.u64()?,
            });
        }
        let mut sets = [BTreeSet::new(), BTreeSet::new()];
        for set in &mut sets {
            let n = r.u32()?;
            r.check_count("segments", u64::from(n), u64::from(MAX_SEGS), 8)?;
            for _ in 0..n {
                set.insert(ObjId::from_raw(r.u64()?)?);
            }
        }
        r.finish()?;
        let [condemned, dropped] = sets;
        Ok(Self {
            generation,
            runs,
            condemned,
            dropped,
        })
    }
}
