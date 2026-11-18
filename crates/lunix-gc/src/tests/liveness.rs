//! Liveness arithmetic (gate): per-segment live bytes equal a recount from the
//! model for 1 000 random volumes; the cost-benefit order equals a reference
//! implementation; no candidate at or above the threshold is compacted.

use std::collections::BTreeMap;
use std::time::Duration;

use lunix_format::{ChunkId, Hint, IdLen, SegId};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::entry_hint;
use lunix_iface::store::SegmentStore;

use super::sim::{high_water, MapKind, Sim, SimCfg, GRACE};
use crate::compact::{self, Candidate};

fn next(z: &mut u64) -> u64 {
    *z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut x = *z;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The model's live extents per segment, recounted from the roots, the
/// footers and the index without `lunix-gc`.
fn recount(s: &Sim) -> BTreeMap<SegId, u64> {
    let lean = s.cfg.id_len == IdLen::Zero;
    let footer_id = |h: &Hint| -> Option<ChunkId> {
        s.store
            .footer(h.seg)
            .ok()?
            .entries
            .iter()
            .find(|x| x.off == h.off && x.clen == h.clen)
            .map(|x| x.chunk_id)
    };
    let mut live: std::collections::HashSet<Hint> = Default::default();
    for (_, root) in s.roots() {
        let mut es = Vec::new();
        s.map.walk_leaves(&root, &mut |_, e| es.push(*e)).unwrap();
        for e in es {
            let h = entry_hint(&e);
            if lean {
                live.insert(h);
                continue;
            }
            match footer_id(&h).filter(|id| e.prefix.matches(id)) {
                Some(id) => match s.index.get(&id).unwrap() {
                    Some(c) if footer_id(&c.hint) == Some(id) => {
                        live.insert(c.hint);
                    }
                    _ => {
                        live.insert(h);
                    }
                },
                None => {
                    for c in s.index.get_by_prefix(&e.prefix).unwrap() {
                        if footer_id(&c.hint) == Some(c.id) {
                            live.insert(c.hint);
                        }
                    }
                }
            }
        }
    }
    let mut per = BTreeMap::new();
    for h in live {
        *per.entry(h.seg).or_insert(0u64) += u64::from(h.clen);
    }
    per
}

/// The reference cost-benefit order: score `(1-u)·age/(1+u)` computed
/// directly, descending, ties by id.
fn reference(
    s: &Sim,
    per: &BTreeMap<SegId, u64>,
    extents: &dyn Fn(SegId) -> u64,
    threshold: f64,
) -> Vec<SegId> {
    let hwm = high_water(&s.retained);
    let mut v: Vec<(f64, SegId)> = SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .filter(|x| x.seg.raw() <= hwm)
        .filter_map(|x| {
            let live = *per.get(&x.seg)?;
            let u = live as f64 / x.size_bytes as f64;
            let age = (hwm - x.seg.raw()) as f64;
            // An output holds the payload plus 128 bytes per extent and
            // 512 bytes per segment, and must be 1 KiB smaller.
            let out = live + extents(x.seg) * 128 + 512;
            (u < threshold && x.size_bytes >= out + 1024)
                .then_some(((1.0 - u) * age / (1.0 + u), x.seg))
        })
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|(_, s)| s).collect()
}

#[test]
fn per_segment_live_bytes_equal_a_model_recount_for_1000_volumes() {
    let volumes: u64 = std::env::var("LUNIX_GC_VOLUMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let mut compared = 0u64;
    let mut with_candidates = 0u64;
    for vol in 0..volumes {
        let mut z = vol;
        let id_len = [IdLen::Sixteen, IdLen::ThirtyTwo, IdLen::Zero][(vol % 3) as usize];
        let mut cfg = SimCfg::new(if vol % 2 == 0 { 16 << 10 } else { 64 << 10 }, id_len);
        cfg.map = if vol % 5 == 0 {
            MapKind::Radix
        } else {
            MapKind::Flat
        };
        cfg.retain_last = 1 + (vol % 4) as u32;
        let mut s = Sim::new(cfg);
        let commits = 2 + next(&mut z) % 8;
        for _ in 0..commits {
            for _ in 0..(next(&mut z) % 24) {
                s.write(next(&mut z) % 48, next(&mut z) % 20);
            }
            s.commit().unwrap();
            if next(&mut z).is_multiple_of(3) {
                // Passes that compact leave stale hints and relocations.
                s.clock.advance(GRACE);
                s.pass().unwrap();
            }
        }
        if next(&mut z).is_multiple_of(2) {
            s.pin(next(&mut z));
        }
        let l = s.with_view(|v| crate::liveness(v, s.cfg.gc)).unwrap();
        let want = recount(&s);
        assert_eq!(l.seg_live, want, "volume {vol}: per-segment live bytes");
        assert_eq!(l.unresolved, 0, "volume {vol}");
        compared += want.len() as u64;
        // Ranking against the reference, with nothing excluded.
        let got: Vec<SegId> = compact::candidates(&l, 0.5, high_water(&s.retained), &|_| false)
            .iter()
            .map(|c| c.seg)
            .collect();
        assert_eq!(
            got,
            reference(&s, &want, &|seg| l.live_in(seg).len() as u64, 0.5),
            "volume {vol}: candidate order"
        );
        with_candidates += u64::from(!got.is_empty());
    }
    eprintln!(
        "liveness: {volumes} volumes, {compared} segments compared, {with_candidates} with compaction candidates"
    );
    // GC's retained set reaches one window further down (N24), which keeps
    // more alive on these short random histories than the engine's window.
    assert!(with_candidates > volumes / 20);
}

#[test]
fn rank_orders_by_cost_benefit_then_id() {
    let c = |seg: u64, size: u64, live: u64, age: u64| Candidate {
        seg: SegId::from_raw(seg).unwrap(),
        size,
        live,
        age,
    };
    let mut v = vec![
        c(5, 100, 40, 10),
        c(3, 100, 10, 10),
        c(4, 100, 10, 10),
        c(9, 100, 10, 1),
        c(1, 100, 49, 100),
    ];
    compact::rank(&mut v);
    let order: Vec<u64> = v.iter().map(|x| x.seg.raw()).collect();
    // scores: 1: .51*100/1.49=34.2; 3,4: .9*10/1.1=8.18; 5: .6*10/1.4=4.29; 9: .818
    assert_eq!(order, vec![1, 3, 4, 5, 9]);
}

#[test]
fn nothing_at_or_above_the_threshold_is_compacted() {
    for vol in 0..60u64 {
        let mut z = vol + 7;
        let mut s = Sim::new(SimCfg::new(16 << 10, IdLen::Sixteen));
        for _ in 0..12 {
            for _ in 0..(next(&mut z) % 20) {
                s.write(next(&mut z) % 48, next(&mut z) % 30);
            }
            s.commit().unwrap();
            s.clock.advance(Duration::from_secs(5));
            let l = s.with_view(|v| crate::liveness(v, s.cfg.gc)).unwrap();
            let sizes: BTreeMap<SegId, u64> = SegmentStore::list(&s.store)
                .unwrap()
                .iter()
                .map(|x| (x.seg, x.size_bytes))
                .collect();
            s.pass().unwrap();
            for c in s.gc.last_compacted() {
                let u = l.live_bytes(c.seg) as f64 / sizes[&c.seg] as f64;
                assert!(u < 0.5, "segment {:?} compacted at u = {u}", c.seg);
                assert!(l.live_bytes(c.seg) > 0);
            }
            s.commit().unwrap();
        }
    }
}
