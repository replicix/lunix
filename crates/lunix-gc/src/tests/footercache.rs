//! The compactor's footer cache holds the footers of all sources of a queue
//! group: compacting N sources whose extents interleave (the queue is sorted
//! by chunk number) loads each footer about once, not once per extent.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lunix_format::segment::FooterBody;
use lunix_format::{Hint, IdLen, SegId};
use lunix_iface::gc::GcView;
use lunix_iface::store::{ExtentLocation, SegmentInfo, SegmentStore, SegmentWriter};
use lunix_iface::Result;

use super::sim::{Sim, SimCfg};
use crate::compact::{Candidate, Compactor};
use crate::config::GcConfig;
use crate::footers::CacheBytes;
use crate::points::Points;

/// A segment store that counts footer loads.
struct Counting<'a> {
    inner: &'a dyn SegmentStore,
    footers: AtomicU64,
}

impl SegmentStore for Counting<'_> {
    fn open_segment(&self) -> Result<Box<dyn SegmentWriter>> {
        self.inner.open_segment()
    }
    fn read_extent(&self, hint: &Hint, out: &mut Vec<u8>) -> Result<()> {
        self.inner.read_extent(hint, out)
    }
    fn locate(&self, hint: &Hint) -> Result<ExtentLocation> {
        self.inner.locate(hint)
    }
    fn footer(&self, seg: SegId) -> Result<FooterBody> {
        self.footers.fetch_add(1, Ordering::Relaxed);
        self.inner.footer(seg)
    }
    fn list(&self) -> Result<Vec<SegmentInfo>> {
        self.inner.list()
    }
    fn delete(&self, seg: SegId) -> Result<u64> {
        self.inner.delete(seg)
    }
}

/// Source `k` of `n` holds chunks `k, k + n, k + 2n, ...`, so the queue (sorted
/// by chunk) alternates between all sources.
fn interleaved(n: u64, per: u64) -> Sim {
    let mut cfg = SimCfg::new(16 << 10, IdLen::Sixteen);
    cfg.chunk_count = n * per;
    cfg.segment_target = 1 << 30;
    cfg.gc = GcConfig::default();
    let mut s = Sim::new(cfg);
    for k in 0..n {
        for j in 0..per {
            s.write(j * n + k, 1 + j * n + k);
        }
        s.commit().expect("commit");
    }
    for _ in 0..3 {
        s.commit().expect("commit");
    }
    s
}

/// Footer loads of a full compaction of every segment with live data, with
/// the compactor's cache of `cap`.
fn footer_loads(s: &Sim, cap: CacheBytes) -> (u64, u64) {
    let l = s.with_view(|v| crate::liveness(v, s.cfg.gc)).expect("mark");
    let sources: Vec<Candidate> = l
        .live
        .keys()
        .map(|&seg| Candidate {
            seg,
            size: 1,
            live: l.live_bytes(seg),
            age: 0,
        })
        .collect();
    let n = sources.len() as u64;
    let counting = Counting {
        inner: &s.store,
        footers: AtomicU64::new(0),
    };
    let points = Points::new(s.hooks.clone());
    let mut c = Compactor::new(sources, Duration::ZERO, 1 << 16, cap);
    let (mut outbox, mut outputs) = (Vec::new(), Default::default());
    s.with_view(|v| {
        let view = GcView {
            segments: &counting,
            ..*v
        };
        for _ in 0..100_000 {
            if c.is_done() {
                break;
            }
            c.unit(&view, &l, &points, &mut outbox, &mut outputs)
                .expect("unit");
        }
        assert!(c.is_done());
    });
    assert!(!outbox.is_empty(), "something was moved");
    (n, counting.footers.load(Ordering::Relaxed))
}

#[test]
fn compaction_loads_about_one_footer_per_source() {
    let s = interleaved(6, 8);
    let (n, loads) = footer_loads(&s, CacheBytes::new(s.cfg.gc.footer_cache_bytes));
    assert!(n >= 6, "{n} sources");
    // Once per source; one more of slack for a store that lists the output.
    assert!(loads <= n + 1, "{loads} footer loads for {n} sources");
}

/// The unit mismatch this guards (`FooterCache::new(8)` meant 8 segments,
/// then 8 bytes): a cache below one footer reloads almost per extent.
#[test]
fn a_cache_below_one_footer_reloads_per_extent() {
    let s = interleaved(6, 8);
    let (n, loads) = footer_loads(&s, CacheBytes::new(8));
    assert!(loads > 4 * n, "{loads} loads for {n} sources");
}
