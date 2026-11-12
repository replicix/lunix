//! Stress: 8 writers (disjoint and overlapping ranges), 2 readers and a cutter
//! for 10 s (`LUNIX_STRESS_SECS` overrides). Afterwards every cut taken during
//! the run is compared with a serial replay of the writes `R < write_seq <= c`
//! (`R` = the highest `cut_seq` released before that cut: everything `<= R` is
//! gone by design), and the final state with a replay of everything.
//!
//! The cutter follows the engine's protocol (DESIGN 8.1): it picks `c` while no
//! writer is between "assign `write_seq`" and "insert", so every later insert
//! has `write_seq > c`. Writers hold the gate shared, the cutter exclusively.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use lunix_format::WriteSeq;
use lunix_iface::journal::{JournalLoc, LaneId};
use lunix_iface::overlay::{BlockRange, Overlay, PieceSource};
use lunix_overlay::ShardedOverlay;
use lunix_testkit::{coalesce, FlatOverlay};

mod common;
use common::Versioned;

const UNIVERSE: u64 = 4096;
const WRITERS: u64 = 8;

#[derive(Clone, Copy)]
struct Op {
    seq: u64,
    lba: u64,
    blocks: u64,
    hole: bool,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn loc_for(seq: u64) -> JournalLoc {
    JournalLoc {
        lane: LaneId((seq % 8) as u16),
        ordinal: (seq >> 20) as u32,
        offset: 4096 + (seq & 0xfffff) * (1 << 20),
    }
}

#[test]
fn concurrent_writers_readers_and_a_cutter_equal_a_serial_replay() {
    let secs: u64 = std::env::var("LUNIX_STRESS_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let deadline = Instant::now() + Duration::from_secs(secs);
    let chunk_blocks = 16;
    let o = Arc::new(
        ShardedOverlay::with_options(lunix_overlay::OverlayOptions {
            shards: 8,
            stripe_chunks: 2,
            ..lunix_overlay::OverlayOptions::new(chunk_blocks)
        })
        .unwrap(),
    );
    let gate = Arc::new(RwLock::new(()));
    let counter = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));

    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let (o, gate, counter) = (o.clone(), gate.clone(), counter.clone());
            std::thread::spawn(move || {
                let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ ((w + 1) * 0x1000_0001));
                let mut log = Vec::new();
                let region = UNIVERSE / 2 / WRITERS;
                while Instant::now() < deadline {
                    let r = rng.next();
                    // Half the ops stay in the writer's own region, half go
                    // anywhere in the shared upper half.
                    let (base, span) = if r & 1 == 0 {
                        (w * region, region)
                    } else {
                        (UNIVERSE / 2, UNIVERSE / 2)
                    };
                    let blocks = 1 + (r >> 8) % 40.min(span);
                    let lba = base + (r >> 20) % (span - blocks + 1);
                    let hole = (r >> 4).is_multiple_of(6);
                    {
                        let _g = gate.read().unwrap();
                        let seq = counter.fetch_add(1, Ordering::SeqCst) + 1;
                        let range = BlockRange { lba, blocks };
                        if hole {
                            o.insert_hole(range, WriteSeq(seq));
                        } else {
                            o.insert_write(range, loc_for(seq), WriteSeq(seq));
                        }
                        log.push(Op {
                            seq,
                            lba,
                            blocks,
                            hole,
                        });
                    }
                    std::thread::sleep(Duration::from_micros(50));
                }
                log
            })
        })
        .collect();

    let readers: Vec<_> = (0..2u64)
        .map(|i| {
            let (o, stop) = (o.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut rng = Rng(0xdead_beef ^ (i + 1));
                let mut out = Vec::new();
                let mut n = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let r = rng.next();
                    let blocks = 1 + (r >> 8) % 64;
                    let lba = (r >> 20) % (UNIVERSE - blocks + 1);
                    o.lookup(BlockRange { lba, blocks }, &mut out);
                    let mut next = lba;
                    for p in &out {
                        assert_eq!(p.range.lba, next);
                        assert!(p.range.blocks >= 1);
                        if matches!(p.source, PieceSource::None) {
                            assert_eq!(p.write_seq, WriteSeq(0));
                        } else {
                            assert!(p.write_seq.0 >= 1);
                        }
                        next += p.range.blocks;
                    }
                    assert_eq!(next, lba + blocks);
                    n += 1;
                }
                n
            })
        })
        .collect();

    let cutter = {
        let (o, gate, counter) = (o.clone(), gate.clone(), counter.clone());
        std::thread::spawn(move || {
            let mut max_released = 0u64;
            let mut cuts = 0u64;
            let mut taken = Vec::new();
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(7));
                let c = {
                    let _g = gate.write().unwrap();
                    counter.load(Ordering::SeqCst)
                };
                // Writers insert seq > c while we get to `cut`: overwrites of
                // blocks whose version <= c is shadowed by the time of the cut.
                std::thread::yield_now();
                let cut = o.cut(WriteSeq(c));
                let seen = counter.load(Ordering::SeqCst);
                for ch in &cut.chunks {
                    let mut prev_end = ch.chunk * chunk_blocks;
                    for p in &ch.pieces {
                        assert!(p.write_seq.0 <= c);
                        assert!(p.range.lba >= prev_end);
                        prev_end = p.range.lba + p.range.blocks;
                        assert!(prev_end <= (ch.chunk + 1) * chunk_blocks);
                    }
                }
                // Inserts after `c` cannot change a cut at `c`.
                let again = o.cut(WriteSeq(c));
                assert_eq!(
                    again.chunks.iter().map(|x| x.chunk).collect::<Vec<_>>(),
                    cut.chunks.iter().map(|x| x.chunk).collect::<Vec<_>>()
                );
                for (x, y) in again.chunks.iter().zip(&cut.chunks) {
                    assert_eq!(
                        coalesce(&x.pieces),
                        coalesce(&y.pieces),
                        "chunk {}",
                        x.chunk
                    );
                }
                taken.push((c, max_released, seen, cut.clone()));
                if cuts.is_multiple_of(3) {
                    o.release(&cut);
                    max_released = max_released.max(c);
                }
                cuts += 1;
            }
            (max_released, cuts, taken)
        })
    };

    let mut ops: Vec<Op> = writers
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    let (max_released, cuts, taken) = cutter.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    let reads: u64 = readers.into_iter().map(|h| h.join().unwrap()).sum();
    eprintln!(
        "stress: {} writes, {} cuts, {} lookups, released up to {}",
        ops.len(),
        cuts,
        reads,
        max_released
    );
    assert!(ops.len() > 1000 && cuts > 5 && reads > 1000);

    o.validate().unwrap();
    ops.sort_by_key(|op| op.seq);
    let apply = |ov: &dyn Overlay, op: &Op| {
        let range = BlockRange {
            lba: op.lba,
            blocks: op.blocks,
        };
        if op.hole {
            ov.insert_hole(range, WriteSeq(op.seq));
        } else {
            ov.insert_write(range, loc_for(op.seq), WriteSeq(op.seq));
        }
    };

    // Every cut equals the replay of its writes.
    let mut overwritten_before_cut = 0usize;
    for (c, r, seen, cut) in &taken {
        let replay = FlatOverlay::new(chunk_blocks);
        for op in ops.iter().filter(|op| op.seq > *r && op.seq <= *c) {
            apply(&replay, op);
        }
        let want = replay.cut(WriteSeq(*c));
        assert_eq!(cut.cut_seq, want.cut_seq);
        assert_eq!(
            cut.chunks.iter().map(|x| x.chunk).collect::<Vec<_>>(),
            want.chunks.iter().map(|x| x.chunk).collect::<Vec<_>>(),
            "cut {c} (released up to {r})"
        );
        for (x, y) in cut.chunks.iter().zip(&want.chunks) {
            assert_eq!(
                coalesce(&x.pieces),
                coalesce(&y.pieces),
                "cut {c}, chunk {}",
                x.chunk
            );
        }
        // Writes with seq > c had been issued by the time the cut returned.
        if seen > c {
            overwritten_before_cut += 1;
        }
    }
    eprintln!(
        "stress: {} cuts verified ({overwritten_before_cut} with later writes)",
        taken.len()
    );

    // The final state equals a replay of everything, released up to the last
    // release; versions included (dirty bytes).
    let ver = Versioned::new(chunk_blocks);
    for op in &ops {
        apply(&ver, op);
    }
    ver.release(&ver.cut(WriteSeq(max_released)));
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let all = BlockRange {
        lba: 0,
        blocks: UNIVERSE,
    };
    o.lookup(all, &mut a);
    ver.lookup(all, &mut b);
    assert_eq!(coalesce(&a), coalesce(&b));
    assert_eq!(o.dirty_bytes(), ver.dirty_bytes());
}
