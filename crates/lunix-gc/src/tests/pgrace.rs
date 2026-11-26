//! Milestone pgrace: the short grace of condemned segments under pressure
//! (ADR-32 amendment, rule change F3; `Collector::set_segment_grace`).
//!
//! The rule: while the engine reports pressure and asked for a short segment
//! grace, a condemned **segment** may be deleted by a mark that began at least
//! that grace after the segment's own flag. Map packs, index runs and commit
//! objects keep the grace in force (the floor under pressure). The recheck is
//! still the only judge. Timelines are in milliseconds after `base` on the
//! view's manual clock; the grace in force under pressure is the 30 s floor.
//!
//! Negative controls (`MutantOn`): the scope test fails if packs and commits
//! get the short grace, the onset test fails if a segment's clock starts at
//! the pressure onset instead of its own flag.

use std::time::Duration;

use lunix_iface::clock::Clock;
use lunix_iface::gc::Collector;

use super::recheck::{at, listed_segments, make_dead, pass_now, reclaim, segs};
use super::sim::{Sim, SimCfg};
use crate::collector::{MutantOn, PgraceMutant};
use crate::condemned::ObjectRef;
use crate::config::{self, GcConfig};

const CS: u32 = 16 << 10;
/// The grace under pressure the engine hands the collector (the floor).
const FLOOR: Duration = Duration::from_secs(30);
/// The short grace of the scenarios.
const G: Duration = Duration::from_millis(5_000);

fn pg_cfg() -> SimCfg {
    let mut cfg = SimCfg::new(CS, lunix_format::IdLen::Sixteen);
    cfg.chunk_count = 64;
    cfg.gc = GcConfig {
        pass_interval: Duration::from_secs(1),
        busy_interval: Duration::from_secs(1),
        fast_recheck: Some(Duration::from_secs(1)),
        segment_grace_min: Duration::ZERO,
        ..GcConfig::daemon()
    };
    cfg
}

/// Pressure with the short grace in force, as the engine sets it per call.
fn pressure(s: &mut Sim, short: Option<Duration>) {
    s.grace = FLOOR;
    s.gc.set_pressure(true);
    s.gc.set_segment_grace(short);
}

fn others(s: &Sim) -> Vec<ObjectRef> {
    s.gc.condemned()
        .into_iter()
        .filter(|o| !matches!(o, ObjectRef::Segment(_)))
        .collect()
}

/// A volume with condemned segments (and condemned packs or commit objects)
/// flagged at 1 000 ms, under pressure from the start.
struct World {
    s: Sim,
    base: Duration,
    segs: Vec<ObjectRef>,
    others: Vec<ObjectRef>,
}

const FLAGGED: u64 = 1_000;

fn world(short: Option<Duration>) -> World {
    let mut s = Sim::new(pg_cfg());
    let base = s.clock.monotonic();
    pressure(&mut s, short);
    make_dead(&mut s, 0, 6, 1_000);
    at(&mut s, base, FLAGGED);
    pass_now(&mut s);
    let segs = listed_segments(&s);
    let others = others(&s);
    assert!(!segs.is_empty(), "the first pass condemned segments");
    World {
        s,
        base,
        segs,
        others,
    }
}

impl World {
    /// A pass whose mark begins `ms` after the flags: a new tip makes the
    /// regular pass due (the gaps are 1 s).
    fn pass_at(&mut self, ms: u64) {
        at(&mut self.s, self.base, FLAGGED + ms);
        self.s.write(40, 9_000 + ms);
        self.s.commit().unwrap();
        pass_now(&mut self.s);
    }

    fn listed(&self, o: &ObjectRef) -> bool {
        self.s.gc.condemned().contains(o)
    }
}

/// The scope scenario: segments go at their short grace, everything else at
/// the grace in force (30 s). `Err` says what went wrong.
fn scope() -> Result<(), String> {
    let mut w = world(Some(G));
    if w.others.is_empty() {
        return Err("no pack or commit object was condemned: the scenario proves nothing".into());
    }
    w.pass_at(G.as_millis() as u64);
    if let Some(o) = w.segs.iter().find(|o| w.listed(o)) {
        return Err(format!("{o} was not deleted at its short grace"));
    }
    if let Some(o) = w.others.iter().find(|o| !w.listed(o)) {
        return Err(format!("{o} was deleted before the grace in force"));
    }
    w.pass_at(FLOOR.as_millis() as u64 - 1);
    if let Some(o) = w.others.iter().find(|o| !w.listed(o)) {
        return Err(format!("{o} was deleted 1 ms before the grace in force"));
    }
    // At the grace in force they may go (a mark that began then; some wait
    // for the next one for the delete's other rules), and one grace later
    // every one of them has.
    w.pass_at(FLOOR.as_millis() as u64);
    w.pass_at(2 * FLOOR.as_millis() as u64);
    if let Some(o) = w.others.iter().find(|o| w.listed(o)) {
        return Err(format!(
            "{o} was not deleted a grace after the grace in force"
        ));
    }
    w.s.check_roots()?;
    w.s.check_ledger()
}

/// A segment's clock starts at its own flag: segments flagged before the
/// pressure began are covered at the onset, the ones flagged less than the
/// short grace before it are not until their own grace is over.
fn onset() -> Result<(), String> {
    let mut s = Sim::new(pg_cfg());
    let base = s.clock.monotonic();
    s.grace = Duration::from_secs(60);
    make_dead(&mut s, 0, 6, 1_000);
    at(&mut s, base, 1_000);
    pass_now(&mut s);
    let x = listed_segments(&s);
    if x.is_empty() {
        return Err("no X".into());
    }
    // Y: flagged 4 s before the onset, 1 s short of the grace.
    at(&mut s, base, 36_000);
    make_dead(&mut s, 20, 3, 2_000);
    at(&mut s, base, 36_000);
    pass_now(&mut s);
    let y: Vec<_> = listed_segments(&s)
        .into_iter()
        .filter(|o| !x.contains(o))
        .collect();
    if y.is_empty() {
        return Err("no Y".into());
    }
    // The onset at 40 s (the floor in force, the short grace on).
    at(&mut s, base, 40_000);
    pressure(&mut s, Some(G));
    pass_now(&mut s);
    if let Some(o) = x.iter().find(|o| s.gc.condemned().contains(o)) {
        return Err(format!(
            "{o} (flagged 39 s ago) was not covered at the onset"
        ));
    }
    if let Some(o) = y.iter().find(|o| !s.gc.condemned().contains(o)) {
        return Err(format!(
            "{o} (flagged 4 s ago) was deleted before its own grace"
        ));
    }
    // Y's grace ended at 41 s; the delay of the recheck-only pass is 1 s.
    at(&mut s, base, 42_000);
    pass_now(&mut s);
    if let Some(o) = y.iter().find(|o| s.gc.condemned().contains(o)) {
        return Err(format!("{o} was not deleted once its own grace was over"));
    }
    s.check_roots()?;
    s.check_ledger()
}

// ------------------------------------------------------------- the rule

/// (a) A segment flagged at `t` is deferred by a mark that began 1 ms before
/// `t + g` and deleted by one that began at `t + g`.
#[test]
fn a_segment_is_deferred_until_its_short_grace_and_deleted_at_it() {
    let mut a = world(Some(G));
    a.pass_at(G.as_millis() as u64 - 1);
    for o in &a.segs {
        assert!(a.listed(o), "{o} deleted 1 ms early");
    }
    let mut b = world(Some(G));
    b.pass_at(G.as_millis() as u64);
    for o in &b.segs {
        assert!(!b.listed(o), "{o} not deleted at its grace");
        if let ObjectRef::Segment(g) = o {
            assert!(!segs(&b.s).contains(g));
        }
    }
    b.s.check_roots().unwrap();
    b.s.check_ledger().unwrap();
}

/// (b) Map packs and commit objects flagged with the segments wait for the
/// grace in force, not the short one.
#[test]
fn packs_and_commits_keep_the_floor() {
    scope().unwrap();
}

/// Negative control of (b): with the short grace for every kind of object the
/// scope scenario fails.
#[test]
fn control_scope_fails_if_packs_and_commits_get_the_short_grace() {
    let _m = MutantOn::new(PgraceMutant::AllObjects);
    let e = scope().expect_err("the control must fail");
    assert!(e.contains("deleted before the grace in force"), "{e}");
}

/// (c) Without pressure the key does nothing, whatever the engine says first.
#[test]
fn without_pressure_the_short_grace_does_nothing() {
    let mut s = Sim::new(pg_cfg());
    let base = s.clock.monotonic();
    s.gc.set_pressure(false);
    s.gc.set_segment_grace(Some(G));
    make_dead(&mut s, 0, 6, 1_000);
    at(&mut s, base, 1_000);
    pass_now(&mut s);
    let x = listed_segments(&s);
    assert!(!x.is_empty());
    // The short grace sent before the pressure is not remembered by it.
    s.gc.set_pressure(true);
    s.gc.set_segment_grace(None);
    s.gc.set_pressure(false);
    for ms in [G.as_millis() as u64, 20_000, 59_000] {
        at(&mut s, base, 1_000 + ms);
        s.write(40, 9_000 + ms);
        s.commit().unwrap();
        pass_now(&mut s);
        for o in &x {
            assert!(s.gc.condemned().contains(o), "{o} deleted at {ms} ms");
        }
    }
    assert_eq!(s.gc.status().segment_grace_ms, 60_000);
    at(&mut s, base, 61_000);
    s.write(40, 9_999);
    s.commit().unwrap();
    pass_now(&mut s);
    for o in &x {
        assert!(!s.gc.condemned().contains(o), "{o} stays after the grace");
    }
    assert_eq!(reclaim(&s).pgrace_deletes, 0);
}

/// (d) The onset: the rule is per segment and counts from its own flag.
#[test]
fn the_backlog_is_covered_at_the_onset_each_segment_from_its_own_flag() {
    onset().unwrap();
}

/// Negative control of (d): a clock that starts at the onset leaves the
/// backlog waiting, and the scenario fails.
#[test]
fn control_onset_fails_if_the_clock_starts_at_the_pressure_onset() {
    let _m = MutantOn::new(PgraceMutant::OnsetClock);
    let e = onset().expect_err("the control must fail");
    assert!(e.contains("was not covered at the onset"), "{e}");
}

/// (e) Pressure ends between the trigger and the verdict: the segment waits
/// for the grace then in force, and the deferral is not a delete.
#[test]
fn a_pressure_that_ends_before_the_verdict_leaves_the_longer_grace_in_force() {
    let mut w = world(Some(G));
    at(&mut w.s, w.base, FLAGGED + G.as_millis() as u64 + 1_000);
    // The trigger (a recheck-only pass is due) is true; the engine's next
    // call says the pressure is over.
    w.s.grace = Duration::from_secs(60);
    w.s.gc.set_pressure(false);
    w.s.gc.set_segment_grace(None);
    pass_now(&mut w.s);
    for o in &w.segs {
        assert!(w.listed(o), "{o} deleted without the pressure");
    }
    at(&mut w.s, w.base, FLAGGED + 60_000);
    w.s.write(40, 9_100);
    w.s.commit().unwrap();
    pass_now(&mut w.s);
    for o in &w.segs {
        assert!(!w.listed(o), "{o} deleted under the configured grace");
    }
    w.s.check_roots().unwrap();
    w.s.check_ledger().unwrap();
}

/// (f) The settings validation still refuses a grace below the floor: the
/// short grace never travels in `GcParams`.
#[test]
fn the_validation_still_refuses_a_grace_below_the_floor() {
    let s = Sim::new(pg_cfg());
    let mut p = s.params();
    p.grace = config::grace_floor(&p);
    assert!(config::validate(&p).is_ok());
    p.grace = G;
    assert!(config::validate(&p).is_err());
}

/// The collector holds its own minimum: a grace below it is raised.
#[test]
fn the_collector_raises_a_grace_below_its_minimum() {
    let mut cfg = pg_cfg();
    cfg.gc.segment_grace_min = Duration::from_secs(2);
    let mut w = {
        let mut s = Sim::new(cfg);
        let base = s.clock.monotonic();
        pressure(&mut s, Some(Duration::ZERO));
        make_dead(&mut s, 0, 6, 1_000);
        at(&mut s, base, FLAGGED);
        pass_now(&mut s);
        let segs = listed_segments(&s);
        World {
            s,
            base,
            segs,
            others: Vec::new(),
        }
    };
    w.pass_at(1_999);
    for o in &w.segs {
        assert!(w.listed(o), "{o} deleted below the minimum");
    }
    assert_eq!(w.s.gc.status().segment_grace_ms, 2_000);
}

/// (g) The urgent pass judges segments with the short grace under pressure
/// and everything else with the floor.
#[test]
fn the_urgent_pass_deletes_segments_at_the_short_grace_and_nothing_else() {
    let mut w = world(Some(G));
    at(&mut w.s, w.base, FLAGGED + 4_999);
    let p = w.s.urgent(u64::MAX).unwrap();
    assert!(p.done);
    for o in &w.segs {
        assert!(w.listed(o), "{o} deleted 1 ms early by the urgent pass");
    }
    at(&mut w.s, w.base, FLAGGED + 5_000);
    let p = w.s.urgent(u64::MAX).unwrap();
    assert!(p.freed > 0);
    for o in &w.segs {
        assert!(!w.listed(o), "{o} survived the urgent pass");
    }
    for o in &w.others {
        assert!(
            w.listed(o),
            "{o} deleted by the urgent pass before the floor"
        );
    }
    // Without the short grace the same pass leaves everything.
    let mut n = world(None);
    at(&mut n.s, n.base, FLAGGED + 5_000);
    n.s.urgent(u64::MAX).unwrap();
    for o in &n.segs {
        assert!(n.listed(o));
    }
    w.s.check_roots().unwrap();
    w.s.check_ledger().unwrap();
}

/// (h) The counters and the unchanged evidence phrase.
#[test]
fn the_pgrace_figures_count_only_the_deletes_the_short_grace_allowed() {
    let mut w = world(Some(G));
    assert_eq!(w.s.gc.status().segment_grace_ms, 5_000);
    w.pass_at(G.as_millis() as u64);
    let r = reclaim(&w.s);
    let deleted = w.segs.len() as u64;
    assert_eq!(r.pgrace_deletes, deleted);
    assert!(r.pgrace_bytes > 0);
    assert_eq!(r.pgrace_bytes, w.s.gc.stats().bytes_freed);
    assert_eq!(r.pgrace_min_wait_ms, 5_000, "{r:?}");
    assert_eq!(r.pgrace_uncondemned, 0);
    // The packs and commits that go at the floor are not counted.
    w.pass_at(FLOOR.as_millis() as u64);
    assert_eq!(reclaim(&w.s).pgrace_deletes, deleted);
    // No new reason string: the delete lines keep the old phrase.
    let text = format!("{:?}", w.s.gc.status().last_pass);
    assert!(!text.contains("pressure grace"), "{text}");
}

/// (h) A wrong proposal is un-condemned by the recheck at the short grace,
/// and nothing live is deleted.
#[test]
fn the_recheck_un_condemns_a_live_segment_offered_under_the_short_grace() {
    let mut cfg = pg_cfg();
    cfg.gc.propose_unfiltered = true;
    let mut s = Sim::new(cfg);
    let base = s.clock.monotonic();
    pressure(&mut s, Some(G));
    for c in 0..6 {
        s.write(c, 100 + c);
    }
    s.commit().unwrap();
    for _ in 0..4 {
        s.commit().unwrap();
    }
    // The first call loads the collector, as an engine step does.
    pass_now(&mut s);
    at(&mut s, base, 1_000);
    let offer = s.all_segments();
    assert!(!offer.is_empty());
    s.propose(&offer).unwrap();
    assert!(!listed_segments(&s).is_empty(), "the proposal was taken");
    at(&mut s, base, 1_000 + G.as_millis() as u64);
    s.write(40, 9_000);
    s.commit().unwrap();
    pass_now(&mut s);
    let r = reclaim(&s);
    assert!(r.pgrace_uncondemned > 0, "{r:?}");
    assert_eq!(r.pgrace_deletes, 0, "nothing live was deleted: {r:?}");
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

/// (i) A restart starts every grace again, at the first call of the new
/// collector, with the grace in force then.
#[test]
fn a_restart_starts_the_short_grace_again() {
    let mut w = world(Some(G));
    at(&mut w.s, w.base, FLAGGED + 3_000);
    w.s.crash_reopen(7).unwrap();
    w.s.check_roots().unwrap();
    pressure(&mut w.s, Some(G));
    // The first call after the reopen loads the list: the graces start now.
    let first = w.s.clock.monotonic();
    w.s.step().unwrap();
    assert!(!listed_segments(&w.s).is_empty(), "the list survived");
    let listed: Vec<ObjectRef> = listed_segments(&w.s);
    at(&mut w.s, first, G.as_millis() as u64 - 1);
    w.s.write(40, 9_200);
    w.s.commit().unwrap();
    pass_now(&mut w.s);
    for o in &listed {
        assert!(w.s.gc.condemned().contains(o), "{o} deleted 1 ms early");
    }
    at(&mut w.s, first, G.as_millis() as u64 + 1_000);
    w.s.write(40, 9_201);
    w.s.commit().unwrap();
    pass_now(&mut w.s);
    for o in &listed {
        assert!(!w.s.gc.condemned().contains(o), "{o} not deleted");
    }
    w.s.check_roots().unwrap();
    w.s.check_ledger().unwrap();
}
