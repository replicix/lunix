//! The candidate matrix: every candidate policy of the lead's notes that the
//! model can express, run over seeds at two store-amplification levels, as
//! markdown tables.

use std::fmt::Write as _;

use anyhow::Result;

use crate::calibration::{run_seeds, Spread};
use crate::report::RunReport;

/// A candidate: a policy name and the overrides that make it.
pub struct Cand {
    /// The row's name.
    pub name: &'static str,
    /// The policy.
    pub policy: &'static str,
    /// `key=value` overrides (`policy.key` for the policy).
    pub sets: &'static [(&'static str, &'static str)],
}

const fn c(
    name: &'static str,
    policy: &'static str,
    sets: &'static [(&'static str, &'static str)],
) -> Cand {
    Cand { name, policy, sets }
}

/// The candidates. P7 (cap from the last stop exit) changes the gate's grant
/// loop and has no row (a simulator-side emulation does nothing); P5/P6 need the collector's forecast and have none.
pub const CANDIDATES: &[Cand] = &[
    c("base (main)", "main", &[]),
    c("gate-gc", "gate-gc", &[]),
    c("leadzone", "leadzone", &[]),
    c(
        "P0a bound compressed",
        "main",
        &[("bound_mode", "compressed")],
    ),
    c("P0b true-up early", "main", &[("bound_mode", "early")]),
    c(
        "P0c limit anchored on disk",
        "main",
        &[("anchor_disk", "true")],
    ),
    c(
        "P1 pace in store bytes",
        "tunable",
        &[("policy.p1", "true")],
    ),
    c(
        "P2 growth pass 1 GiB",
        "tunable",
        &[("policy.growth_gib", "1")],
    ),
    c(
        "P2 growth pass 0.25 GiB",
        "tunable",
        &[("policy.growth_gib", "0.25")],
    ),
    c(
        "G9 grace from death",
        "main",
        &[("grace_from_death", "true")],
    ),
    c("P3 grace 15 s", "tunable", &[("policy.grace_s", "15")]),
    c("P3 grace 10 s", "tunable", &[("policy.grace_s", "10")]),
    c("P3 grace 5 s", "tunable", &[("policy.grace_s", "5")]),
    c("P3 grace 2 s", "tunable", &[("policy.grace_s", "2")]),
    c("P3 grace 0 s", "tunable", &[("policy.grace_s", "-1")]),
    c(
        "P4 grace 5 s + gap 1 s",
        "tunable",
        &[("policy.grace_s", "5"), ("policy.busy_interval_s", "1")],
    ),
    c(
        "P4 grace 0 s + gap 1 s",
        "tunable",
        &[("policy.grace_s", "-1"), ("policy.busy_interval_s", "1")],
    ),
    c(
        "G5 threshold from headroom",
        "tunable",
        &[("policy.g5", "true")],
    ),
    c("G1 batched index work", "main", &[("batch_index", "true")]),
    c(
        "G3 incremental mark",
        "main",
        &[("incremental_mark", "true")],
    ),
    c("G4 two collector threads", "main", &[("gc_workers", "2")]),
    c(
        "G1+G3+G4",
        "main",
        &[
            ("batch_index", "true"),
            ("incremental_mark", "true"),
            ("gc_workers", "2"),
        ],
    ),
    c(
        "G7 punch, grace 30 s",
        "main",
        &[("punch", "true"), ("compaction", "false")],
    ),
    c(
        "G7 punch, grace 5 s",
        "tunable",
        &[
            ("punch", "true"),
            ("compaction", "false"),
            ("policy.grace_s", "5"),
        ],
    ),
    c(
        "P5 time-to-limit pacing",
        "tunable",
        &[("policy.p5", "true")],
    ),
    c("P6 ETA floor", "tunable", &[("policy.p6", "true")]),
    c(
        "G2 condemn at each cut",
        "main",
        &[("condemn_at_cut", "true")],
    ),
    c(
        "P1' + P6 (ETA floor)",
        "tunable",
        &[("policy.p1", "true"), ("policy.p6", "true")],
    ),
    c(
        "C6 gap 1 s only",
        "tunable",
        &[("policy.busy_interval_s", "1")],
    ),
    c(
        "C5 condemn at cut + gap 1 s",
        "tunable",
        &[("condemn_at_cut", "true"), ("policy.busy_interval_s", "1")],
    ),
    c(
        "C5+C7 condemn at cut, gap 1 s, grace 2 s",
        "tunable",
        &[
            ("condemn_at_cut", "true"),
            ("policy.busy_interval_s", "1"),
            ("policy.grace_s", "2"),
        ],
    ),
    c(
        "P1' + C5 + gap 1 s",
        "tunable",
        &[
            ("policy.p1", "true"),
            ("condemn_at_cut", "true"),
            ("policy.busy_interval_s", "1"),
        ],
    ),
    c(
        "combo A: P2 + grace 5 s + gap 1 s",
        "tunable",
        &[
            ("policy.growth_gib", "1"),
            ("policy.grace_s", "5"),
            ("policy.busy_interval_s", "1"),
        ],
    ),
    c(
        "combo B: A + P0a",
        "tunable",
        &[
            ("policy.growth_gib", "1"),
            ("policy.grace_s", "5"),
            ("policy.busy_interval_s", "1"),
            ("bound_mode", "compressed"),
        ],
    ),
    c(
        "combo C: P2 + P1",
        "tunable",
        &[("policy.growth_gib", "1"), ("policy.p1", "true")],
    ),
    c(
        "combo D: P2 + G9 + P0a",
        "tunable",
        &[
            ("policy.growth_gib", "1"),
            ("grace_from_death", "true"),
            ("bound_mode", "compressed"),
        ],
    ),
    c(
        "combo E: G7 + P2 + grace 5 s",
        "tunable",
        &[
            ("punch", "true"),
            ("compaction", "false"),
            ("policy.growth_gib", "1"),
            ("policy.grace_s", "5"),
            ("policy.busy_interval_s", "1"),
        ],
    ),
];

fn p90(runs: &[RunReport]) -> f64 {
    let mut v: Vec<f64> = runs
        .iter()
        .map(|r| r.cells.iter().map(|c| c.max).fold(0.0, f64::max))
        .collect();
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * 0.9).ceil() as usize)
        .saturating_sub(1)
        .min(v.len() - 1)]
}

fn med(runs: &[RunReport], f: impl Fn(&RunReport) -> f64) -> f64 {
    Spread::of(runs.iter().map(f).collect()).median
}

/// One table: the scenario at one amplification level.
///
/// # Errors
/// As [`run_seeds`].
pub fn table(
    scenario: &str,
    merge: &str,
    n: u64,
    extra: &[(String, String)],
    round3: bool,
) -> Result<String> {
    let list: &[Cand] = if round3 { ROUND3 } else { CANDIDATES };
    let mut out = String::new();
    let _ = writeln!(out, "| candidate | stops ff / steady | first stop s | longest med / p90 s | p99.9 s | IOPS ff / steady | burst IOPS | peak used/limit | amp | whole-dead / moved GB | gc busy | dwell s | dead det / grace / recheck / delete s |");
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for cand in list {
        let mut sets: Vec<(String, String)> = vec![("merge".into(), merge.into())];
        sets.extend(extra.iter().cloned());
        for (k, v) in cand.sets {
            sets.push(((*k).into(), (*v).into()));
        }
        let runs = run_seeds(scenario, cand.policy, &sets, n, true)?;
        let first: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.cells.first().and_then(|c| c.first_stop_s))
            .collect();
        let fs = if first.is_empty() {
            "none".to_string()
        } else {
            format!(
                "{:.0} ({}/{})",
                Spread::of(first.clone()).median,
                first.len(),
                runs.len()
            )
        };
        let dw: Vec<f64> = runs
            .iter()
            .filter_map(|r| Some(r.first_stop_abs? - r.band_entry?.1))
            .collect();
        let dwell = if dw.is_empty() {
            "-".to_string()
        } else {
            format!("{:.1}", Spread::of(dw).median)
        };
        let _ = writeln!(
            out,
            "| {} | {:.0} / {:.0} | {} | {:.1} / {:.1} | {:.2} | {:.1}K / {:.1}K | {:.1}K | {:.3} | {:.1} | {:.1} / {:.1} | {:.0} % | {} | {:.1} / {:.1} / {:.1} / {:.1} |",
            cand.name,
            med(&runs, |r| r.first_fill.stops as f64),
            med(&runs, |r| r.steady.stops as f64),
            fs,
            med(&runs, |r| r.cells.iter().map(|c| c.max).fold(0.0, f64::max)),
            p90(&runs),
            med(&runs, |r| r.cells.first().map_or(0.0, |c| c.p999)),
            med(&runs, |r| r.first_fill.iops) / 1e3,
            med(&runs, |r| r.steady.iops) / 1e3,
            med(&runs, |r| r.cells.first().map_or(0.0, |c| c.burst_iops)) / 1e3,
            med(&runs, |r| r.peak_used_over_limit),
            med(&runs, |r| r.amp_pre_band.unwrap_or(r.amp_total)),
            med(&runs, |r| r.reclaim_whole) / 1e9,
            med(&runs, |r| r.reclaim_moved) / 1e9,
            med(&runs, |r| r.gc_share) * 100.0,
            dwell,
            med(&runs, |r| r.dead.retention + r.dead.detection),
            med(&runs, |r| r.dead.grace),
            med(&runs, |r| r.dead.recheck),
            med(&runs, |r| r.dead.delete),
        );
    }
    Ok(out)
}

/// The round-3 rows: the missing combinations, the gap split, C5 with its
/// cost, and a lagging, biased estimator.
pub const ROUND3: &[Cand] = &[
    c("base (main)", "main", &[]),
    c("P1' alone", "tunable", &[("policy.p1", "true")]),
    c(
        "P1' alone, lagged A, bias +15 %",
        "tunable",
        &[
            ("policy.p1", "true"),
            ("policy.wobble", "false"),
            ("a_lag", "true"),
            ("a_bias", "0.15"),
        ],
    ),
    c(
        "P1' alone, lagged A, bias -15 %",
        "tunable",
        &[
            ("policy.p1", "true"),
            ("policy.wobble", "false"),
            ("a_lag", "true"),
            ("a_bias", "-0.15"),
        ],
    ),
    c("C5 free", "main", &[("condemn_at_cut", "true")]),
    c(
        "C5 costed (list sync, flags, skipped while a pass runs)",
        "main",
        &[("condemn_at_cut", "true"), ("c5_cost", "true")],
    ),
    c(
        "C5 costed, disk (5.5 ms flags, 3 ms sync)",
        "main",
        &[
            ("condemn_at_cut", "true"),
            ("c5_cost", "true"),
            ("condemn_ms", "5.5"),
            ("list_sync_ms", "3"),
        ],
    ),
    c(
        "C5 costed, 20 % proposals fail",
        "main",
        &[
            ("condemn_at_cut", "true"),
            ("c5_cost", "true"),
            ("c5_error", "0.2"),
        ],
    ),
    c(
        "gap 1 s (collector pass gap)",
        "tunable",
        &[("policy.busy_interval_s", "1")],
    ),
    c(
        "gap (a) fast recheck-only pass",
        "tunable",
        &[("policy.fast_recheck", "true")],
    ),
    c("gap (b) cut cadence 1 s", "main", &[("cut_cadence_s", "1")]),
    c("gap (b) cut cadence 2 s", "main", &[("cut_cadence_s", "2")]),
    c("gap (b) cut cadence 5 s", "main", &[("cut_cadence_s", "5")]),
    c(
        "P1' + C5 free (no gap)",
        "tunable",
        &[("policy.p1", "true"), ("condemn_at_cut", "true")],
    ),
    c(
        "P1' + gap 1 s (no C5)",
        "tunable",
        &[("policy.p1", "true"), ("policy.busy_interval_s", "1")],
    ),
    c(
        "P1' + C5 free + gap 1 s",
        "tunable",
        &[
            ("policy.p1", "true"),
            ("condemn_at_cut", "true"),
            ("policy.busy_interval_s", "1"),
        ],
    ),
    c(
        "P1' + C5 costed + gap 1 s",
        "tunable",
        &[
            ("policy.p1", "true"),
            ("condemn_at_cut", "true"),
            ("c5_cost", "true"),
            ("policy.busy_interval_s", "1"),
        ],
    ),
    c(
        "P1' + C5 free + (a) fast recheck",
        "tunable",
        &[
            ("policy.p1", "true"),
            ("condemn_at_cut", "true"),
            ("policy.fast_recheck", "true"),
        ],
    ),
    c(
        "P1' + C5 free + (b) cadence 2 s",
        "tunable",
        &[
            ("policy.p1", "true"),
            ("condemn_at_cut", "true"),
            ("cut_cadence_s", "2"),
        ],
    ),
    c(
        "P1' + (b) cadence 2 s (no C5)",
        "tunable",
        &[("policy.p1", "true"), ("cut_cadence_s", "2")],
    ),
];
