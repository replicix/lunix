//! The pipelined schedule (`GcConfig::pipeline`, milestone gcrate): a
//! scheduled pass does not wait out the grace; its mark is the recheck of
//! the listed objects whose grace ended before it began, and a pass starts
//! when a grace ends. A requested pass still runs the whole sequence.

use std::time::Duration;

use lunix_format::IdLen;
use lunix_iface::gc::{Collector, GcState};
use lunix_iface::store::GcStateStore;

use super::sim::{Sim, SimCfg, GRACE};
use crate::condemned::ObjectRef;
use crate::config::GcConfig;

fn sim() -> Sim {
    let mut cfg = SimCfg::new(16 << 10, IdLen::Sixteen);
    cfg.gc = GcConfig {
        pass_interval: Duration::from_secs(30),
        busy_interval: Duration::from_secs(2),
        ..GcConfig::daemon()
    };
    Sim::new(cfg)
}

/// Overwrites the same chunks `rounds` times, one commit per round.
fn churn(s: &mut Sim, rounds: u64, base: u64) {
    for r in 0..rounds {
        for i in 0..12 {
            s.write(i, base + r * 100 + i);
        }
        s.commit().unwrap();
    }
}

/// Steps one pass to its end without ever advancing the clock (a pipelined
/// pass has no grace to wait for); returns the states it went through.
fn pass_now(s: &mut Sim) -> Vec<GcState> {
    let mut states = Vec::new();
    for _ in 0..100_000 {
        let p = s.step().unwrap();
        states.push(p.state);
        if p.done || p.state == GcState::Idle {
            return states;
        }
    }
    panic!("a pass did not finish");
}

fn segments_listed(s: &Sim) -> Vec<ObjectRef> {
    s.gc.condemned()
        .into_iter()
        .filter(|o| matches!(o, ObjectRef::Segment(_)))
        .collect()
}

#[test]
fn a_scheduled_pass_condemns_without_waiting_and_a_later_one_deletes_after_the_grace() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    // The first pass: no grace wait, no delete; the dead are listed.
    let states = pass_now(&mut s);
    assert!(!states.contains(&GcState::Grace), "{states:?}");
    let condemned = segments_listed(&s);
    assert!(!condemned.is_empty());
    assert_eq!(s.gc.stats().bytes_freed, 0);
    // Passes before the grace ended delete nothing listed.
    for _ in 0..5 {
        s.clock.advance(Duration::from_secs(5));
        churn(&mut s, 1, 1000);
        pass_now(&mut s);
        for o in &condemned {
            assert!(s.gc.condemned().contains(o), "{o} went before its grace");
        }
    }
    assert_eq!(s.gc.stats().bytes_freed, 0);
    s.check_roots().unwrap();
    // Once the grace is over the next mark is the recheck: deleted.
    s.clock.advance(GRACE);
    churn(&mut s, 1, 2000);
    pass_now(&mut s);
    for o in &condemned {
        assert!(!s.gc.condemned().contains(o), "{o} is still listed");
    }
    assert!(s.gc.stats().bytes_freed > 0);
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}

#[test]
fn a_pass_starts_when_a_grace_ends_without_a_new_tip() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    pass_now(&mut s);
    assert!(!segments_listed(&s).is_empty());
    // At most one more pass follows the busy one (it changed something);
    // after it, nothing is due until the earliest grace ends.
    s.clock.advance(Duration::from_secs(3));
    pass_now(&mut s);
    let passes = s.gc.stats().passes;
    let evidence = s.store.evidence_passes().unwrap().len();
    for _ in 0..10 {
        s.clock.advance(Duration::from_secs(4));
        let p = s.step().unwrap();
        assert_eq!(p.state, GcState::Idle);
    }
    assert_eq!(s.gc.stats().passes, passes);
    // A pass that only finds graces running writes no evidence object.
    assert_eq!(s.store.evidence_passes().unwrap().len(), evidence);
    // The grace ends (well inside `pass_interval` of the last pass): a pass
    // starts and deletes, although the tip did not change.
    s.clock.advance(GRACE - Duration::from_secs(40));
    pass_now(&mut s);
    assert_eq!(s.gc.stats().passes, passes + 1);
    assert!(s.gc.stats().bytes_freed > 0);
    assert!(segments_listed(&s).is_empty());
    s.check_roots().unwrap();
}

#[test]
fn a_requested_pass_still_waits_out_its_grace_and_deletes() {
    let mut s = sim();
    churn(&mut s, 6, 0);
    s.clock.advance(Duration::from_secs(1));
    assert!(s.gc.request_pass());
    let p = s.step().unwrap();
    assert_eq!(p.state, GcState::Grace);
    s.pass().unwrap();
    assert_eq!(s.gc.stats().passes, 1);
    assert!(s.gc.stats().bytes_freed > 0);
    assert!(segments_listed(&s).is_empty());
    s.check_roots().unwrap();
    s.check_ledger().unwrap();
}
