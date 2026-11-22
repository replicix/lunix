//! The calibration regression: `main` on the measured cells keeps the
//! figures it was calibrated to (deterministic seeds, no wall-clock asserts),
//! and the two null-result policies stay no better than `main` there.

use lunix_spacesim::calibration::{run_seeds, summarize};

const SEEDS: u64 = 4;

fn runs(scenario: &str, policy: &str) -> Vec<lunix_spacesim::RunReport> {
    run_seeds(scenario, policy, &[], SEEDS, true).unwrap()
}

#[test]
fn a_run_is_deterministic_for_a_seed() {
    let a = run_seeds("ram-64k", "main", &[], 1, true).unwrap();
    let b = run_seeds("ram-64k", "main", &[], 1, true).unwrap();
    assert_eq!(a[0].trace_csv(), b[0].trace_csv());
    assert_eq!(a[0].summary(), b[0].summary());
}

#[test]
fn ram_64k_main_has_the_measured_first_fill() {
    let r = runs("ram-64k", "main");
    for run in &r {
        // Measured: reclaim_rate = 0 at the first band entry (12 of 12
        // volumes of the leadzone report); anything under 50 MB/s is "none"
        // beside the 200-650 MB/s of the band.
        let (rate, _) = run.band_entry.expect("the volume reaches the band");
        assert!(rate < 50e6, "reclaim at band entry {rate}");
        // Store growth per admitted byte before the band: measured about
        // 2.8; the model gives about 4 with cuts of the measured size (a
        // known gap, see the report).
        let a = run.amp_pre_band.expect("amplification before the band");
        assert!((2.2..=5.0).contains(&a), "amplification {a}");
        // The grace under pressure is the floor.
        assert!((run.dead.grace - 30.0).abs() < 1e-6, "{:?}", run.dead);
        // The first delete is after a grace, never before.
        let (c, d) = (run.first_condemn_s.unwrap(), run.first_delete_s.unwrap());
        assert!(d - c >= 30.0, "condemn {c} delete {d}");
    }
    // Measured: the first stop comes 12-15 s into the overwrite (20-27 s by
    // the t6t8 series) and the first cell stops in every run.
    for run in &r {
        let c = &run.cells[0];
        let first = c.first_stop_s.expect("the first cell stops");
        assert!((8.0..=27.0).contains(&first), "first stop at {first}");
        assert!(c.stops >= 1);
    }
    let s = summarize(&r);
    // Measured: 1-7 stops per cell; the first fill stalls for most of a
    // grace (maximum completion 18-33 s).
    assert!(s.stops.max >= 1.0 && s.stops.max <= 12.0, "{:?}", s.stops);
    assert!(
        s.max_completion.max >= 15.0 && s.max_completion.max <= 70.0,
        "{:?}",
        s.max_completion
    );
    // Measured sustained rate 24-25 K IOPS (T6: 21-33 K): within 40 %.
    assert!(
        (10_000.0..=40_000.0).contains(&s.iops.median),
        "{:?}",
        s.iops
    );
}

#[test]
fn gate_gc_and_leadzone_are_no_better_than_main() {
    let main = summarize(&runs("ram-64k", "main"));
    for p in ["gate-gc", "leadzone"] {
        let s = summarize(&runs("ram-64k", p));
        // The null results: the longest completion and the stops do not fall
        // by a fifth, and the first band entry still reclaims nothing.
        assert!(
            s.max_completion.max >= 0.8 * main.max_completion.max,
            "{p}: {:?} against {:?}",
            s.max_completion,
            main.max_completion
        );
        assert!(s.stops.median + 1.0 >= 0.8 * main.stops.median, "{p}");
        assert!(s.reclaim_at_entry.max < 50.0, "{p}");
    }
}

#[test]
fn the_network_volume_never_engages_the_gate() {
    for scenario in ["netvol-64k", "netvol-16k"] {
        let r = runs(scenario, "main");
        for run in &r {
            assert!(run.band_entry.is_none(), "{scenario} entered the band");
            for c in &run.cells {
                assert_eq!(c.stops, 0, "{scenario}");
                assert_eq!(c.delayed_writes, 0, "{scenario}");
                assert_eq!(c.enospc, 0, "{scenario}");
            }
        }
    }
}
