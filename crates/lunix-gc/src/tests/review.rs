//! Directed cases from the M17.A review: B1 (a failed `drop_segment` after
//! the unlink), M1 (`--truncate-tip` needs the predecessor's `chain_floor`
//! commit), M5 (re-flagging a listed segment, the `outputs` half of the
//! delete-time invariant, confirming index copies against footers), the grace
//! on the normal path, and the re-judge of a commit at delete time.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lunix_format::local::RollbackFloor;
use lunix_format::{ChunkId, CommitSeq, ExtentKind, Hint, IdLen, SegId};
use lunix_iface::clock::Clock;
use lunix_iface::crypto::{CommitCodec, ExtentCodec};
use lunix_iface::gc::{Collector, GcState};
use lunix_iface::index::ChunkIndex;
use lunix_iface::map::ChunkNo;
use lunix_iface::store::{CommitStore, SegmentStore, VolumeDir};
use lunix_iface::{Error, Result};

use super::sim::{data_of, high_water, Sim, SimCfg, GRACE};
use crate::condemned::ObjectRef;
use crate::{FloorSource, Gc};

const CS: u32 = 16 << 10;

fn sim() -> Sim {
    Sim::new(SimCfg::new(CS, IdLen::Sixteen))
}

fn segs(s: &Sim) -> BTreeSet<SegId> {
    SegmentStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|x| x.seg)
        .collect()
}

fn id_of(seed: u64) -> ChunkId {
    lunix_testkit::PlainCodec::new().chunk_id(ExtentKind::Data, &data_of(seed, CS))
}

fn hint_at(s: &Sim, ci: u64) -> Hint {
    let e = s.map.get(ChunkNo(ci)).unwrap().expect("populated");
    lunix_iface::map::entry_hint(&e)
}

/// A segment holding only chunk id X (seed 77) that no root references.
fn dead_x(s: &mut Sim) -> SegId {
    s.write(0, 77);
    s.commit().unwrap();
    let seg = hint_at(s, 0).seg;
    s.write(0, 78);
    for _ in 0..3 {
        s.commit().unwrap();
    }
    seg
}

/// B1: a transient error of `drop_segment` after the unlink. Later passes
/// must drop the entries before clearing the flag, so dedup never returns
/// the deleted segment.
#[test]
fn a_drop_segment_error_after_the_unlink_never_lets_dedup_reach_the_deleted_segment() {
    for nospace in [false, true] {
        let mut s = sim();
        let x_seg = dead_x(&mut s);
        s.clock.advance(Duration::from_secs(5));
        s.arm_fault("index.drop.before", 1, nospace);
        let mut failed = false;
        for _ in 0..200 {
            match s.step() {
                Ok(p) if p.done => break,
                Ok(_) => {}
                Err(_) => {
                    failed = true;
                    break;
                }
            }
            s.clock.advance(Duration::from_secs(10));
        }
        s.disarm_faults();
        assert!(failed, "the injected drop_segment error fired");
        assert!(!segs(&s).contains(&x_seg), "the file is gone");
        // The flag is still set: dedup does not see the stale entries.
        assert!(s.index.get_for_dedup(&id_of(77)).unwrap().is_none());
        for _ in 0..6 {
            s.clock.advance(GRACE * 2);
            s.commit().unwrap();
            s.pass().unwrap();
        }
        assert!(s.gc.condemned().is_empty());
        assert!(
            s.index.get(&id_of(77)).unwrap().is_none(),
            "entries dropped"
        );
        assert!(s.index.get_for_dedup(&id_of(77)).unwrap().is_none());
        s.write(5, 77);
        s.commit().unwrap();
        assert!(segs(&s).contains(&hint_at(&s, 5).seg));
        s.check_roots().unwrap();
        s.check_ledger().unwrap();
    }
}

/// M1 (N12): after `fsck --repair --truncate-tip` the predecessor is the tip
/// and mount verifies the chain down to its own `chain_floor`.
#[test]
fn truncate_tip_finds_the_predecessors_chain_floor_commit() {
    let mut s = sim();
    for i in 0..12u64 {
        s.write(i % 8, 100 + i);
        s.commit().unwrap();
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
    }
    s.quiesce().unwrap();
    let tip = s.tip.common.commit_seq;
    let pred = match &s.retained[s.retained.len() - 2] {
        lunix_format::commit::Commit::Full(f) => f.clone(),
        _ => unreachable!(),
    };
    assert_eq!(pred.common.commit_seq.0, tip.0 - 1);
    assert!(pred.chain_floor < s.tip.chain_floor);
    // Commits below the lowest retained chain_floor did go.
    assert!(!s.commit_exists(pred.chain_floor.0 - 1));
    // The truncation: the tip is set aside, the predecessor verifies.
    s.store.set_aside(tip).unwrap();
    assert_eq!(s.store.tip().unwrap(), Some(pred.common.commit_seq));
    s.commit_codec
        .verify_chain(
            &s.store,
            pred.common.commit_seq,
            pred.chain_floor,
            &[0x11; 32],
        )
        .unwrap();
}

/// N12 (b): GC keeps every FULL from the lowest `chain_floor` a retained
/// FULL records as a root. After `--truncate-tip` the predecessor mounts
/// (its chain verifies down to its own `chain_floor`) and every block of
/// every root of its retained window reads back the model's content, also
/// the older root whose data the dropped tip had overwritten.
#[test]
fn truncate_tip_then_mount_reads_every_root_of_the_predecessors_window() {
    let mut s = sim();
    for i in 0..12u64 {
        s.write(i % 4, 100 + i);
        s.write(4 + i % 3, 200 + i);
        s.commit().unwrap();
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
    }
    // Passes without commits: everything dead is condemned and deleted.
    for _ in 0..4 {
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
    }
    s.check_roots().unwrap();
    let tip = s.tip.common.commit_seq;
    let pred = match &s.retained[s.retained.len() - 2] {
        lunix_format::commit::Commit::Full(f) => f.clone(),
        _ => unreachable!(),
    };
    let older = pred.chain_floor;
    assert_eq!(older.0, tip.0 - 2);
    assert_ne!(
        s.model[&older.0], s.model[&tip.0],
        "the dropped tip overwrote data of the older root"
    );
    // The truncation as fsck does it, then a mount (crash image, sweep,
    // catch-up): the predecessor is the tip.
    s.store.set_aside(tip).unwrap();
    s.raise_floor = false;
    s.crash_reopen(7).unwrap();
    assert_eq!(s.tip.common.commit_seq, pred.common.commit_seq);
    s.commit_codec
        .verify_chain(&s.store, pred.common.commit_seq, older, &[0x11; 32])
        .unwrap();
    let roots: Vec<u64> = s.roots().iter().map(|(q, _)| *q).collect();
    assert!(roots.contains(&older.0), "roots {roots:?}");
    s.check_roots_with(&*s.index).unwrap();
    // GC runs at once on the truncated volume and keeps it whole.
    for i in 0..4u64 {
        s.write(i, 900 + i);
        s.commit().unwrap();
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
        s.check_roots().unwrap();
    }
}

/// M5: a crash between the list write and the index flags; after the reopen
/// the listed segment is flagged again before its recheck, so a dedup hit
/// taken after the recheck mark cannot land on a segment that is then
/// deleted.
#[test]
fn a_listed_segment_without_a_flag_after_a_crash_is_reflagged() {
    let mut s = sim();
    let x_seg = dead_x(&mut s);
    s.clock.advance(Duration::from_secs(5));
    s.hooks.arm("gc.condemn.after_list_write", 1);
    let mut crashed = false;
    for _ in 0..200 {
        if s.step().is_err() {
            crashed = true;
            break;
        }
        s.clock.advance(Duration::from_secs(1));
    }
    assert!(crashed && s.hooks.crashed().is_some());
    s.crash_reopen(1).unwrap();
    assert!(segs(&s).contains(&x_seg));
    s.commit().unwrap();
    let mut reached = false;
    for _ in 0..100_000 {
        let p = s.unit().unwrap();
        if p.state == GcState::Deleting {
            reached = true;
            break;
        }
        if p.done {
            break;
        }
        if p.state == GcState::Grace {
            s.clock.advance(GRACE * 2);
        } else {
            s.clock.advance(Duration::from_millis(100));
        }
    }
    assert!(reached);
    assert!(
        s.index.get_for_dedup(&id_of(77)).unwrap().is_none(),
        "flagged again"
    );
    s.write(5, 77);
    s.commit().unwrap();
    assert_ne!(hint_at(&s, 5).seg, x_seg);
    for _ in 0..2 {
        s.clock.advance(GRACE * 2);
        s.commit().unwrap();
        s.pass().unwrap();
    }
    s.check_roots().unwrap();
}

/// M5: the `outputs` half of the delete-time invariant. An output below the
/// mark (a FULL without the moves lifted it) holds the live copies; the
/// source must outlive it across a crash.
#[test]
fn the_source_outlives_an_unnamed_output_below_the_mark_across_a_crash() {
    let mut s = sim();
    for i in 0..4 {
        s.write(i, 200 + i);
    }
    s.commit().unwrap();
    let src = hint_at(&s, 0).seg;
    for i in 1..4 {
        s.write(i, 300 + i);
    }
    for _ in 0..3 {
        s.commit().unwrap();
    }
    s.clock.advance(Duration::from_secs(5));
    s.pass().unwrap();
    assert!(!s.moved.is_empty(), "compacted");
    let out = s.moved[0].new.seg;
    let moved = std::mem::take(&mut s.moved);
    s.discard(3);
    s.commit().unwrap();
    assert!(out.raw() < high_water(&s.retained));
    s.moved = moved;
    for _ in 0..4 {
        s.clock.advance(GRACE * 2);
        s.urgent(u64::MAX).unwrap();
    }
    assert!(segs(&s).contains(&src));
    s.crash_reopen(3).unwrap();
    s.check_roots().unwrap();
}

/// M5: an index entry for a live id that names a missing segment, or the
/// extent of another chunk, is not trusted: the hinted copy stays live and
/// its segment is kept.
#[test]
fn an_unconfirmed_index_copy_never_makes_the_hinted_copy_dead() {
    for wrong in ["missing segment", "another chunk"] {
        let mut s = sim();
        s.write(0, 1);
        s.commit().unwrap();
        let x_hint = hint_at(&s, 0);
        s.write(1, 2);
        s.commit().unwrap();
        let y_hint = hint_at(&s, 1);
        assert_ne!(x_hint.seg, y_hint.seg);
        let bogus = match wrong {
            "missing segment" => Hint {
                seg: SegId::from_raw(x_hint.seg.raw() + 1000).unwrap(),
                off: 0,
                clen: x_hint.clen,
            },
            _ => y_hint,
        };
        // As a persisted run after a crash between an unlink and the drop, or
        // a damaged (only checksummed) run would leave it.
        s.index.insert_hinted(id_of(1), bogus);
        for _ in 0..4 {
            s.clock.advance(GRACE * 2);
            s.commit().unwrap();
            s.pass().unwrap();
        }
        assert!(
            segs(&s).contains(&x_hint.seg),
            "{wrong}: the hinted copy's segment stays"
        );
        s.check_roots().unwrap();
    }
}

/// The grace on the normal path: nothing condemned is deleted before
/// `gc_grace` has run since the flags were set, however many steps run.
#[test]
fn nothing_is_deleted_before_the_grace_since_the_flag() {
    let mut s = sim();
    let x_seg = dead_x(&mut s);
    s.clock.advance(Duration::from_secs(5));
    let mut flagged_at = None;
    for _ in 0..10_000 {
        let p = s.unit().unwrap();
        if flagged_at.is_none() && p.state == GcState::Grace {
            flagged_at = Some(s.clock.now());
        }
        if let Some(t) = flagged_at {
            let since = Duration::from_millis(s.clock.now().0 - t.0);
            if since < GRACE {
                assert!(
                    segs(&s).contains(&x_seg),
                    "deleted {since:?} after the flag"
                );
            }
        }
        if p.done {
            break;
        }
        s.clock.advance(Duration::from_secs(1));
    }
    assert!(flagged_at.is_some());
    assert!(!segs(&s).contains(&x_seg), "deleted after the grace");
}

/// A floor source that starts failing on demand.
struct Flaky {
    fail: AtomicBool,
    store: lunix_testkit::MemStore,
}

impl FloorSource for Flaky {
    fn current_floor(&self) -> Result<Option<RollbackFloor>> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Corrupt {
                what: "pins",
                detail: "test",
            });
        }
        VolumeDir::rollback_floor(&self.store)
    }
}

/// The delete of a commit is judged again in its own step: a floor that
/// becomes unreadable after the recheck mark defers it.
#[test]
fn a_floor_that_fails_after_the_recheck_still_keeps_the_commits() {
    let mut s = sim();
    let flaky = Arc::new(Flaky {
        fail: AtomicBool::new(false),
        store: s.store.clone(),
    });
    s.gc = Gc::new(flaky.clone(), s.cfg.gc);
    for i in 0..8 {
        s.write(i, i);
        s.commit().unwrap();
    }
    let before: Vec<u64> = CommitStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|c| c.seq.0)
        .collect();
    s.clock.advance(Duration::from_secs(5));
    let mut reached = false;
    for _ in 0..100_000 {
        let p = s.unit().unwrap();
        if p.state == GcState::Deleting {
            reached = true;
            break;
        }
        s.clock.advance(Duration::from_secs(5));
    }
    assert!(reached);
    assert!(s
        .gc
        .condemned()
        .iter()
        .any(|o| matches!(o, ObjectRef::Commit(_))));
    flaky.fail.store(true, Ordering::SeqCst);
    for _ in 0..10_000 {
        let p = s.unit().unwrap();
        if p.done {
            break;
        }
    }
    let after: Vec<u64> = CommitStore::list(&s.store)
        .unwrap()
        .iter()
        .map(|c| c.seq.0)
        .collect();
    assert_eq!(
        before, after,
        "no commit deleted while the floor is unreadable"
    );
    flaky.fail.store(false, Ordering::SeqCst);
    s.quiesce().unwrap();
    assert!(!s.commit_exists(1));
    let _ = CommitSeq(0);
}

/// M4: an extent that does not open is skipped and reported; the pass, and
/// every later one, still finishes and deletes what it can.
#[test]
fn an_extent_that_does_not_open_is_skipped_not_fatal() {
    let mut s = sim();
    for i in 0..4 {
        s.write(i, 900 + i);
    }
    s.commit().unwrap();
    let src = hint_at(&s, 0).seg;
    for i in 1..4 {
        s.write(i, 950 + i);
    }
    for _ in 0..3 {
        s.commit().unwrap();
    }
    // The codec of the collector's view refuses every extent from now on.
    let refusing = RefusingCodec(lunix_testkit::PlainCodec::new());
    let mut gc = std::mem::replace(&mut s.gc, Gc::new(Arc::new(s.store.clone()), s.cfg.gc));
    for round in 0..3 {
        s.clock.advance(GRACE * 2);
        let mut done = false;
        for _ in 0..10_000 {
            let p = s
                .with_view(|v| {
                    let view = lunix_iface::gc::GcView {
                        extents: &refusing,
                        ..*v
                    };
                    gc.step(&view, Sim::budget())
                })
                .unwrap_or_else(|e| panic!("round {round}: {e}"));
            if p.done {
                done = true;
                break;
            }
            s.clock.advance(Duration::from_secs(5));
        }
        assert!(done);
        s.commit().unwrap();
    }
    assert_eq!(gc.skipped_sources(), vec![src]);
    s.gc = gc;
    s.check_roots().unwrap();
}

struct RefusingCodec(lunix_testkit::PlainCodec);

impl ExtentCodec for RefusingCodec {
    fn mode(&self) -> lunix_format::CryptoMode {
        self.0.mode()
    }
    fn chunk_id(&self, kind: ExtentKind, data: &[u8]) -> ChunkId {
        self.0.chunk_id(kind, data)
    }
    fn seal(
        &self,
        kind: ExtentKind,
        id: &ChunkId,
        desc: &lunix_iface::crypto::EncodingDesc,
        compressed: &[u8],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        self.0.seal(kind, id, desc, compressed, out)
    }
    fn open(
        &self,
        _kind: ExtentKind,
        _bytes: &[u8],
        _expect: &lunix_iface::crypto::OpenExpect<'_>,
        _out: &mut Vec<u8>,
    ) -> Result<lunix_iface::crypto::OpenedMeta> {
        Err(Error::Auth { what: "extent tag" })
    }
}

/// M4: the urgent pass reports the bytes it freed even when it stops on an
/// error afterwards, and the evidence of its deletes is written.
#[test]
fn an_urgent_pass_that_freed_bytes_reports_them_despite_a_later_error() {
    let mut s = sim();
    for r in 0..6u64 {
        for i in 0..12 {
            s.write(i, r * 100 + i);
        }
        s.commit().unwrap();
    }
    s.clock.advance(Duration::from_secs(5));
    s.urgent(u64::MAX).unwrap();
    s.clock.advance(Duration::from_secs(31));
    let passes_before = lunix_iface::store::GcStateStore::evidence_passes(&s.store)
        .unwrap()
        .len();
    // The second delete fails.
    s.arm_fault("pack.delete.before", 2, false);
    let r = s.urgent(u64::MAX);
    s.disarm_faults();
    let p = r.expect("freed bytes are reported");
    assert!(p.freed > 0);
    let passes_after = lunix_iface::store::GcStateStore::evidence_passes(&s.store)
        .unwrap()
        .len();
    assert!(passes_after > passes_before, "evidence written");
    s.quiesce().unwrap();
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// Non-crash faults at every store and index point a collector call passes,
/// across workloads; every root reads afterwards and the volume converges.
#[test]
fn a_fault_at_every_point_is_survived() {
    use super::oracle::{check_quiescent, faulty_steps, FAULT_POINTS};
    for (pi, _) in FAULT_POINTS.iter().enumerate() {
        for w in 0..6u64 {
            let mut s = sim();
            let mut z = (pi as u64) << 16 | w;
            let mut next = || {
                z = z
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                z >> 33
            };
            for round in 0..12 {
                for _ in 0..8 {
                    s.write(next() % 48, next() % 20);
                }
                s.commit().unwrap();
                if round % 3 == 2 {
                    s.clock.advance(GRACE);
                    faulty_steps(&mut s, pi, 1 + next() % 4, next() % 2 == 0, 12).unwrap();
                }
            }
            check_quiescent(&mut s).unwrap_or_else(|e| panic!("{}: {e:?}", FAULT_POINTS[pi]));
        }
    }
}

/// B-1 of the M17.B review: a compaction source must outlive the FULL that
/// first names its output. Only the tip names the output; `--truncate-tip`
/// drops the tip and the mount sweep deletes the output (above the new
/// chain's mark), so the source is the only copy the older roots have.
#[test]
fn a_compaction_source_outlives_the_tip_that_alone_names_its_output() {
    let mut s = sim();
    for ci in 0..4 {
        s.write(ci, 300 + ci);
    }
    s.commit().unwrap();
    let src = s
        .map
        .get(ChunkNo(0))
        .unwrap()
        .map(|e| lunix_iface::map::entry_hint(&e).seg)
        .unwrap();
    for ci in 1..4 {
        s.write(ci, 400 + ci);
    }
    s.commit().unwrap();
    // Age the FULL that holds all four chunks out of GC's retained set.
    for _ in 0..4 {
        s.commit().unwrap();
    }
    s.clock.advance(GRACE * 2);
    s.pass().unwrap();
    assert!(!s.moved.is_empty(), "the source was compacted");
    assert!(s.moved.iter().all(|m| m.old.seg == src));
    let out = s.moved[0].new.seg;
    // The tip names the output and re-hints the map; no later commit.
    s.commit().unwrap();
    for _ in 0..4 {
        s.clock.advance(GRACE * 2);
        s.pass().unwrap();
    }
    assert!(
        segs(&s).contains(&src),
        "the source stays while only the tip names {out:?}"
    );
    // `--truncate-tip`, then the mount.
    let tip = s.tip.common.commit_seq;
    s.store.set_aside(tip).unwrap();
    s.raise_floor = false;
    s.crash_reopen(11).unwrap();
    assert!(
        !segs(&s).contains(&out),
        "the sweep deleted the unnamed output"
    );
    s.check_roots_with(&*s.index).unwrap();
    // One more FULL names the output below the tip: the source goes.
    let mut s2 = sim();
    for ci in 0..4 {
        s2.write(ci, 300 + ci);
    }
    s2.commit().unwrap();
    let src2 = s2
        .map
        .get(ChunkNo(0))
        .unwrap()
        .map(|e| lunix_iface::map::entry_hint(&e).seg)
        .unwrap();
    for ci in 1..4 {
        s2.write(ci, 400 + ci);
    }
    s2.commit().unwrap();
    for _ in 0..4 {
        s2.commit().unwrap();
    }
    s2.clock.advance(GRACE * 2);
    s2.pass().unwrap();
    assert!(!s2.moved.is_empty());
    for _ in 0..5 {
        s2.commit().unwrap();
        s2.clock.advance(GRACE * 2);
        s2.pass().unwrap();
    }
    s2.quiesce().unwrap();
    assert!(
        !segs(&s2).contains(&src2),
        "the source goes once a FULL below the tip names the output"
    );
    s2.check_roots().unwrap();
}
