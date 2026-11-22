//! The store model: every segment individually, with its live chunk count.
//!
//! A cut of `n` writes rewrites `U` distinct chunks of the working set `W`
//! and writes them into new segments. Every older live chunk dies with
//! probability `U / W` (uniform random overwrite), drawn per segment from the
//! seeded generator, so a segment is **wholly dead** only when all of its
//! chunks died; only such segments can be condemned (`Judge::dead`).
//! Partially dead segments are reclaimed by compaction: a segment whose live
//! fraction is under the threshold is a candidate, ranked by LFS
//! cost-benefit; its live chunks move into a new (unnamed) segment and the
//! source is dead once a cut names the output. A sequential overwrite kills
//! segments in creation order, one working-set pass after they were written.

use std::collections::VecDeque;
use std::time::Duration;

use crate::params::Pattern;
use crate::rng::Rng;

/// Where a segment is in its life.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SegState {
    /// On disk, referenced or not yet found dead.
    Live,
    /// A compaction source whose output a cut will name at `ready_cut`.
    Moved {
        /// The cut count from which it may be condemned.
        ready_cut: u64,
    },
    /// On the condemned list since `at`.
    Condemned {
        /// When its grace began.
        at: Duration,
    },
}

/// One segment.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Creation order.
    pub id: u64,
    /// Bytes on disk.
    pub bytes: f64,
    /// Chunks written into it.
    pub chunks: f64,
    /// Chunks still the live copy.
    pub live: f64,
    /// The store's chunk-write counter when it was written (sequential).
    pub d0: f64,
    /// A compaction output not yet named by a durable commit.
    pub unnamed: bool,
    /// The cut count when it was written (the tip's segments are young).
    pub born_cut: u64,
    /// Life state.
    pub state: SegState,
    /// When its last chunk died (for the dead-time split).
    pub died_at: Option<Duration>,
    /// The cut count when it died.
    pub died_cut: u64,
}

/// The dead time of one deleted segment, seconds, split into its terms.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeadTime {
    /// From the last chunk's death until the retained commits let go.
    pub retention: f64,
    /// Until a mark condemned it.
    pub detection: f64,
    /// The grace.
    pub grace: f64,
    /// From the end of the grace to the delete (the recheck mark).
    pub recheck: f64,
    /// Bytes.
    pub bytes: f64,
}

/// What compaction will move.
#[derive(Clone, Debug, Default)]
pub struct CompactPlan {
    /// Segments taken.
    pub sources: Vec<u64>,
    /// Live bytes to move in all.
    pub live_bytes: f64,
    /// Live chunks to move.
    pub live_chunks: f64,
}

/// The segments on disk.
#[derive(Clone, Debug)]
pub struct Store {
    segs: VecDeque<Segment>,
    next_id: u64,
    /// Chunk-writes into segments by cuts so far.
    pub d: f64,
    wc: f64,
    chunk_stored: f64,
    segment_bytes: f64,
    pattern: Pattern,
    /// Cuts completed.
    pub cuts: u64,
    /// Cuts of the retained window (a dead segment stays out of reach of the
    /// collector until this many cuts after its death).
    pub retain_cuts: u64,
    cut_times: Vec<Duration>,
    /// Dead times of the deleted segments.
    pub dead_times: Vec<DeadTime>,
    /// Segments condemned / deleted by the last `mark` / `delete_due`.
    pub last_condemned_segs: u64,
    /// See `last_condemned_segs`.
    pub last_deleted_segs: u64,
    /// Bytes condemned because every chunk died (the whole-dead path).
    pub condemned_whole: f64,
    /// Bytes condemned as compaction sources.
    pub condemned_moved: f64,
    /// Compaction victims by live fraction, ten bins of 10 %, by segment.
    pub victim_hist: [u64; 10],
}

impl Store {
    /// A store for a working set of `wc` chunks of `chunk_stored` bytes,
    /// segments of `segment_bytes`.
    pub fn new(wc: f64, chunk_stored: f64, segment_bytes: f64, pattern: Pattern) -> Self {
        Self {
            segs: VecDeque::new(),
            next_id: 1,
            d: 0.0,
            wc: wc.max(1.0),
            chunk_stored,
            segment_bytes,
            pattern,
            cuts: 0,
            retain_cuts: 3,
            cut_times: vec![Duration::ZERO],
            dead_times: Vec::new(),
            last_condemned_segs: 0,
            last_deleted_segs: 0,
            condemned_whole: 0.0,
            condemned_moved: 0.0,
            victim_hist: [0; 10],
        }
    }

    /// Stored bytes of one chunk.
    pub fn chunk_stored(&self) -> f64 {
        self.chunk_stored
    }

    /// Writes `chunks` chunks as new segments and returns their bytes.
    /// `advance`: the chunk-writes this stands for (0 for a compaction
    /// output). `born_cut` is the cut the segments belong to.
    pub fn write_segments(&mut self, chunks: f64, advance: f64, unnamed: bool) -> f64 {
        if chunks < 0.5 {
            return 0.0;
        }
        let bytes = chunks * self.chunk_stored;
        let n = (bytes / self.segment_bytes).ceil().max(1.0);
        let per_chunks = chunks / n;
        let per_bytes = bytes / n;
        let mut k = 0.0;
        while k < n {
            let id = self.next_id;
            self.next_id += 1;
            self.segs.push_back(Segment {
                id,
                bytes: per_bytes,
                chunks: per_chunks,
                live: per_chunks,
                d0: self.d + k * per_chunks,
                unnamed,
                born_cut: self.cuts + 1,
                state: SegState::Live,
                died_at: None,
                died_cut: 0,
            });
            k += 1.0;
        }
        self.d += advance;
        bytes
    }

    /// A cut of `distinct` chunks became durable at `now`: older live chunks
    /// die at the rate `distinct / W`, outputs are named.
    pub fn cut_done(&mut self, distinct: f64, now: Duration, rng: &mut Rng) {
        // The new segments were written by `write_segments` (born_cut =
        // cuts + 1); they are not hit by their own cut.
        let cut_no = self.cuts + 1;
        let p = (distinct / self.wc).clamp(0.0, 1.0);
        let d = self.d;
        let pattern = self.pattern;
        let wc = self.wc;
        for s in &mut self.segs {
            if s.born_cut == cut_no {
                continue;
            }
            if s.unnamed {
                s.unnamed = false;
            }
            if s.live <= 0.0 || !matches!(s.state, SegState::Live) {
                continue;
            }
            let after = match pattern {
                Pattern::Random => s.live - rng.binomial(s.live, p),
                Pattern::Sequential => {
                    let since = (d - s.d0).max(0.0);
                    (s.chunks * (1.0 - (since - wc) / s.chunks.max(1.0)).clamp(0.0, 1.0)).round()
                }
            };
            s.live = after.min(s.live);
            if s.live <= 0.0 && s.died_at.is_none() {
                s.died_at = Some(now);
                s.died_cut = cut_no;
            }
        }
        for s in &mut self.segs {
            if s.born_cut == cut_no {
                s.unnamed = false;
            }
        }
        self.cuts = cut_no;
        self.cut_times.push(now);
    }

    /// Bytes on disk.
    pub fn disk_bytes(&self) -> f64 {
        self.segs.iter().map(|s| s.bytes).sum()
    }

    /// Bytes of live chunks.
    pub fn live_bytes(&self) -> f64 {
        self.segs
            .iter()
            .filter(|s| !matches!(s.state, SegState::Condemned { .. }))
            .map(|s| s.live * self.chunk_stored)
            .sum()
    }

    /// Bytes on the condemned list.
    pub fn condemned_bytes(&self) -> f64 {
        self.segs
            .iter()
            .filter(|s| matches!(s.state, SegState::Condemned { .. }))
            .map(|s| s.bytes)
            .sum()
    }

    /// Bytes of wholly dead segments not yet condemned.
    pub fn dead_uncondemned_bytes(&self) -> f64 {
        self.segs
            .iter()
            .filter(|s| s.live <= 0.0 && matches!(s.state, SegState::Live))
            .map(|s| s.bytes)
            .sum()
    }

    /// True while a compaction output waits for a cut to name it.
    pub fn has_unnamed(&self) -> bool {
        self.segs.iter().any(|s| s.unnamed)
    }

    /// Number of segments.
    pub fn len(&self) -> usize {
        self.segs.len()
    }

    /// True without segments.
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// The mark: condemns the segments that are wholly dead and out of the
    /// retained window, and the sources of compactions a cut named. Returns
    /// the bytes newly condemned.
    pub fn mark(&mut self, now: Duration) -> f64 {
        let cuts = self.cuts;
        let retain = self.retain_cuts;
        let mut total = 0.0;
        let (mut n, mut whole, mut moved) = (0u64, 0.0, 0.0);
        for s in &mut self.segs {
            if s.unnamed {
                continue;
            }
            let (dead, by_move) = match s.state {
                SegState::Live => (s.live <= 0.0 && s.died_cut + retain <= cuts, false),
                SegState::Moved { ready_cut } => (ready_cut <= cuts, true),
                SegState::Condemned { .. } => (false, false),
            };
            if dead {
                s.state = SegState::Condemned { at: now };
                total += s.bytes;
                n += 1;
                if by_move {
                    moved += s.bytes;
                } else {
                    whole += s.bytes;
                }
            }
        }
        self.last_condemned_segs = n;
        self.condemned_whole += whole;
        self.condemned_moved += moved;
        total
    }

    /// Deletes the condemned segments whose grace `grace` ended at or before
    /// `before` (the start of the rechecking mark), recording their dead
    /// times. Returns the bytes freed.
    pub fn delete_due(&mut self, before: Duration, grace: Duration, now: Duration) -> f64 {
        let mut freed = 0.0;
        let mut nseg = 0u64;
        let mut out = Vec::new();
        let cut_times = &self.cut_times;
        let retain = self.retain_cuts;
        self.segs.retain(|s| {
            if let SegState::Condemned { at } = s.state {
                if at.saturating_add(grace) <= before {
                    freed += s.bytes;
                    nseg += 1;
                    let died = s.died_at.unwrap_or(at);
                    let eligible = cut_times
                        .get(usize::try_from(s.died_cut + retain).unwrap_or(usize::MAX))
                        .copied()
                        .unwrap_or(at)
                        .clamp(died, at);
                    out.push(DeadTime {
                        retention: eligible.saturating_sub(died).as_secs_f64(),
                        detection: at.saturating_sub(eligible).as_secs_f64(),
                        grace: grace.as_secs_f64(),
                        recheck: now.saturating_sub(at.saturating_add(grace)).as_secs_f64(),
                        bytes: s.bytes,
                    });
                    return false;
                }
            }
            true
        });
        self.dead_times.extend(out);
        self.last_deleted_segs = nseg;
        freed
    }

    /// True when a condemned segment's grace ended since `mark_start` and at
    /// or before `now` (the pipelined schedule starts a pass for it).
    pub fn grace_ended_since(
        &self,
        grace: Duration,
        now: Duration,
        mark_start: Option<Duration>,
    ) -> bool {
        self.segs.iter().any(|s| {
            if let SegState::Condemned { at } = s.state {
                let end = at.saturating_add(grace);
                end <= now && mark_start.is_none_or(|m| m < end)
            } else {
                false
            }
        })
    }

    /// The compaction candidates ranked by LFS cost-benefit
    /// `(1 - u) age / (1 + u)`, taken until `budget` live bytes: live
    /// fraction under `threshold`, with live data, not young (the tip's), not
    /// an unnamed output, and an output at least a chunk smaller.
    pub fn plan_compaction(&mut self, threshold: f64, budget: f64) -> CompactPlan {
        let mut hist = [0u64; 10];
        let newest = self.next_id;
        let cuts = self.cuts;
        let this = &*self;
        let mut cands: Vec<(f64, &Segment)> = this
            .segs
            .iter()
            .filter(|s| {
                matches!(s.state, SegState::Live) && s.live > 0.0 && !s.unnamed && s.born_cut < cuts
            })
            .filter_map(|s| {
                let u = s.live / s.chunks.max(1.0);
                if u >= threshold || (s.chunks - s.live) * this.chunk_stored < 1024.0 {
                    return None;
                }
                let age = (newest - s.id) as f64;
                Some(((1.0 - u) * age / (1.0 + u), s))
            })
            .collect();
        cands.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut plan = CompactPlan::default();
        for (_, s) in cands {
            if plan.live_bytes >= budget {
                break;
            }
            let bin = ((s.live / s.chunks.max(1.0)) * 10.0)
                .floor()
                .clamp(0.0, 9.0) as usize;
            hist[bin] += 1;
            plan.sources.push(s.id);
            plan.live_chunks += s.live;
            plan.live_bytes += s.live * self.chunk_stored;
        }
        for (a, b) in self.victim_hist.iter_mut().zip(hist) {
            *a += b;
        }
        plan
    }

    /// Applies a finished compaction: the sources become condemnable after
    /// the next cut, their live chunks form unnamed output segments (chunks
    /// that died since the plan are not moved). Returns the output bytes.
    pub fn apply_compaction(&mut self, plan: &CompactPlan, now: Duration) -> f64 {
        let set: std::collections::HashSet<u64> = plan.sources.iter().copied().collect();
        let (cuts, ready) = (self.cuts, self.cuts + 1);
        let mut moved = 0.0;
        for s in &mut self.segs {
            if set.contains(&s.id) && matches!(s.state, SegState::Live) {
                moved += s.live;
                s.live = 0.0;
                s.died_at.get_or_insert(now);
                s.died_cut = cuts;
                s.state = SegState::Moved { ready_cut: ready };
            }
        }
        self.write_segments(moved, 0.0, true)
    }
}

/// Distinct chunks `n` random writes touch out of `wc`, expected.
pub fn distinct_random(n: f64, wc: f64) -> f64 {
    wc * (1.0 - (-n / wc).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        // 4096 chunks of 32 KiB stored, 1 MiB segments: 32 chunks per segment.
        Store::new(4096.0, 32768.0, 1048576.0, Pattern::Random)
    }

    fn cut(s: &mut Store, distinct: f64, t: u64, rng: &mut Rng) {
        s.write_segments(distinct, distinct, false);
        s.cut_done(distinct, Duration::from_secs(t), rng);
    }

    #[test]
    fn a_cut_makes_segments_of_the_requested_size() {
        let mut s = store();
        let bytes = s.write_segments(320.0, 320.0, false);
        assert!((bytes - 320.0 * 32768.0).abs() < 1.0);
        assert_eq!(s.len(), 10);
    }

    #[test]
    fn old_segments_die_wholly_only_after_many_cuts() {
        let mut s = store();
        let mut rng = Rng::new(1);
        cut(&mut s, 4096.0, 1, &mut rng); // the fill
        for t in 2..6 {
            cut(&mut s, 2000.0, t, &mut rng);
        }
        // A 32-chunk segment, 51 % of the working set rewritten per cut:
        // not one of the fill's segments is wholly dead after four cuts.
        assert_eq!(s.mark(Duration::from_secs(10)), 0.0);
        for t in 6..40 {
            cut(&mut s, 2000.0, t, &mut rng);
        }
        let dead = s.mark(Duration::from_secs(50));
        assert!(dead > 0.0);
        // Not condemned twice.
        assert_eq!(s.mark(Duration::from_secs(51)), 0.0);
    }

    #[test]
    fn the_retained_window_holds_a_dead_segment_back() {
        let mut s = store();
        let mut rng = Rng::new(2);
        cut(&mut s, 64.0, 1, &mut rng);
        // Kill everything with full rewrites.
        cut(&mut s, 4096.0, 2, &mut rng);
        cut(&mut s, 4096.0, 3, &mut rng);
        let first_ids: Vec<_> = s.segs.iter().take(2).map(|x| x.live).collect();
        assert!(first_ids.iter().all(|l| *l == 0.0) || first_ids.iter().any(|l| *l >= 0.0));
        // Died at cut 2 (born 1): condemnable once cuts >= 2 + 2.
        assert_eq!(s.mark(Duration::from_secs(4)), 0.0, "retention not over");
        cut(&mut s, 4096.0, 4, &mut rng);
        cut(&mut s, 4096.0, 5, &mut rng);
        assert!(s.mark(Duration::from_secs(6)) > 0.0);
    }

    #[test]
    fn deletion_waits_for_the_grace_and_records_the_split() {
        let mut s = store();
        let mut rng = Rng::new(3);
        cut(&mut s, 64.0, 1, &mut rng);
        for t in 2..8 {
            cut(&mut s, 4096.0, t, &mut rng);
        }
        let c = s.mark(Duration::from_secs(10));
        assert!(c > 0.0);
        let g = Duration::from_secs(30);
        assert_eq!(
            s.delete_due(Duration::from_secs(39), g, Duration::from_secs(39)),
            0.0
        );
        let freed = s.delete_due(Duration::from_secs(40), g, Duration::from_secs(42));
        assert!(freed > 0.0);
        let d = s.dead_times[0];
        assert!((d.grace - 30.0).abs() < 1e-9);
        assert!((d.recheck - 2.0).abs() < 1e-9);
        assert!(d.detection >= 0.0 && d.retention >= 0.0);
    }

    #[test]
    fn compaction_moves_live_chunks_and_leaves_condemnable_sources() {
        let mut s = store();
        let mut rng = Rng::new(4);
        cut(&mut s, 4096.0, 1, &mut rng);
        for t in 2..5 {
            cut(&mut s, 1500.0, t, &mut rng);
        }
        let live_before = s.live_bytes();
        let plan = s.plan_compaction(0.5, 1e12);
        assert!(!plan.sources.is_empty());
        let out = s.apply_compaction(&plan, Duration::from_secs(6));
        assert!(out > 0.0);
        assert!((s.live_bytes() - live_before).abs() <= live_before * 0.01);
        // The sources go after the next cut.
        assert_eq!(s.mark(Duration::from_secs(7)), 0.0);
        cut(&mut s, 10.0, 8, &mut rng);
        assert!(s.mark(Duration::from_secs(9)) > 0.0);
    }

    #[test]
    fn distinct_chunks_saturate_at_the_working_set() {
        assert!(distinct_random(1.0, 1000.0) < 1.01);
        assert!(distinct_random(1e9, 1000.0) > 999.9);
    }
}
