//! Milestone refs: what the mark reports to the engine's reference table
//! (`GcConfig::tip_refs`, `take_mark_started`, `take_tip_refs`,
//! `take_mark_condemned`).

use std::collections::BTreeMap;
use std::time::Duration;

use lunix_format::{IdLen, SegId};
use lunix_iface::gc::Collector;
use lunix_iface::map::ChunkNo;

use super::sim::{Sim, SimCfg};
use crate::config::GcConfig;

const CS: u32 = 16 << 10;

fn sim(tip_refs: bool) -> Sim {
    let mut cfg = SimCfg::new(CS, IdLen::Sixteen);
    cfg.chunk_count = 64;
    cfg.gc = GcConfig {
        pass_interval: Duration::ZERO,
        tip_refs,
        ..GcConfig::default()
    };
    Sim::new(cfg)
}

/// The entries of the sim's map per segment, counted chunk by chunk.
fn counted(s: &Sim) -> BTreeMap<SegId, u64> {
    let mut out = BTreeMap::new();
    for ci in 0..s.cfg.chunk_count {
        if let Some(e) = s.map.get(ChunkNo(ci)).unwrap() {
            *out.entry(e.seg).or_insert(0) += 1;
        }
    }
    out
}

#[test]
fn a_mark_reports_the_tip_entries_per_segment_the_tip_and_what_it_condemned() {
    let mut s = sim(true);
    // Dedup (the same content in several chunks), overwrites and a hole.
    for c in 0..30u64 {
        s.write(c, c % 5);
    }
    s.commit().unwrap();
    // Segments of one chunk each, then overwritten: dead once the retained
    // window rolled past.
    for i in 0..6u64 {
        s.write(50 + i, 1_000 + i);
        s.commit().unwrap();
    }
    for i in 0..6u64 {
        s.write(50 + i, 2_000 + i);
    }
    s.discard(40);
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(Duration::from_secs(1));
    s.pass().unwrap();
    let started = s.gc.take_mark_started().expect("a mark pinned its roots");
    assert_eq!(started, s.tip.common.commit_seq);
    assert_eq!(s.gc.take_mark_started(), None, "once");
    let t = s.gc.take_tip_refs().expect("a mark finished");
    assert_eq!(t.tip, started);
    let want: Vec<(SegId, u64)> = counted(&s).into_iter().collect();
    assert_eq!(t.refs, want);
    assert!(
        want.iter().any(|&(_, n)| n > 1),
        "dedup put several entries in a segment"
    );
    assert_eq!(s.gc.take_tip_refs(), None, "once");
    let c = s.gc.take_mark_condemned().expect("the mark chose segments");
    assert_eq!(c.tip, started);
    assert!(!c.segs.is_empty(), "the overwritten segments are dead");
    assert!(c.segs.windows(2).all(|w| w[0] < w[1]));
    for seg in &c.segs {
        assert!(!counted(&s).contains_key(seg), "{seg:?} is referenced");
    }
}

#[test]
fn nothing_is_counted_or_reported_without_the_option() {
    let mut s = sim(false);
    for c in 0..10u64 {
        s.write(c, c);
    }
    s.commit().unwrap();
    s.clock.advance(Duration::from_secs(1));
    s.pass().unwrap();
    assert!(
        s.gc.take_mark_started().is_some(),
        "the tip is always noted"
    );
    assert_eq!(s.gc.take_tip_refs(), None);
    assert_eq!(s.gc.take_mark_condemned(), None);
}
