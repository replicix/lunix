//! Running a scenario over several seeds and phases, and the summary figures
//! the calibration compares with the measurements.

use anyhow::Result;

use crate::params::{parse_flat_toml, Scenario};
use crate::policy::{policy_by_name, Policy};
use crate::report::RunReport;
use crate::sim::run;

/// Builds a scenario and a policy from a preset name, `key=value` overrides
/// (keys starting with `policy.` go to the policy) and the policy name.
///
/// # Errors
/// Unknown names, keys or values.
pub fn build(
    scenario: &str,
    policy: &str,
    sets: &[(String, String)],
) -> Result<(Scenario, Box<dyn Policy>)> {
    let mut sc = Scenario::preset(scenario)?;
    let mut pol = policy_by_name(policy)?;
    for (k, v) in sets {
        if let Some(pk) = k.strip_prefix("policy.") {
            pol.set(pk, v)?;
        } else {
            sc.set(k, v)?;
        }
    }
    Ok((sc, pol))
}

/// Parses `key=value` overrides from a flat TOML text.
///
/// # Errors
/// A malformed line.
pub fn sets_from_toml(text: &str) -> Result<Vec<(String, String)>> {
    parse_flat_toml(text)
}

/// Runs `seeds` seeds (1-based) of the scenario; with `phases`, the phase of
/// the collector's cadence is drawn from the seed instead of the preset's.
///
/// # Errors
/// As [`build`] and [`run`].
pub fn run_seeds(
    scenario: &str,
    policy: &str,
    sets: &[(String, String)],
    seeds: u64,
    phases: bool,
) -> Result<Vec<RunReport>> {
    let mut out = Vec::new();
    for seed in 1..=seeds {
        let (mut sc, mut pol) = build(scenario, policy, sets)?;
        sc.seed = seed;
        if phases {
            sc.gc_phase_s = None;
        }
        out.push(run(&sc, pol.as_mut())?);
    }
    Ok(out)
}

/// Median, minimum and maximum of a list.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spread {
    /// The median.
    pub median: f64,
    /// The smallest.
    pub min: f64,
    /// The largest.
    pub max: f64,
}

impl Spread {
    /// The spread of `v` (zero when empty).
    pub fn of(mut v: Vec<f64>) -> Self {
        if v.is_empty() {
            return Self::default();
        }
        v.sort_by(f64::total_cmp);
        Self {
            median: v[v.len() / 2],
            min: v[0],
            max: v[v.len() - 1],
        }
    }
}

/// The figures of a scenario over its runs, per cell and for the run.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// Runs.
    pub runs: usize,
    /// Stops per cell, over every cell of every run.
    pub stops: Spread,
    /// Stops in the first cell.
    pub stops_cell1: Spread,
    /// Longest completion per cell, seconds.
    pub max_completion: Spread,
    /// Longest completion in the first cell.
    pub max_completion_cell1: Spread,
    /// Mean IOPS per cell.
    pub iops: Spread,
    /// Seconds from the cell start to the first stop (first cell).
    pub first_stop: Spread,
    /// Runs whose first cell had a stop.
    pub first_cell_stopped: usize,
    /// Reclaim rate at the first band entry, MB/s.
    pub reclaim_at_entry: Spread,
    /// Store bytes per cut journal byte before the band.
    pub amp_pre_band: Spread,
    /// Whole-run amplification.
    pub amp_total: Spread,
    /// Delayed writes per cell.
    pub delayed: Spread,
    /// Seconds from the overwrite start to the first condemn.
    pub first_condemn: Spread,
    /// Seconds from the overwrite start to the first delete.
    pub first_delete: Spread,
}

/// Summarises the runs.
pub fn summarize(runs: &[RunReport]) -> Summary {
    let mut stops = Vec::new();
    let mut s1 = Vec::new();
    let mut mc = Vec::new();
    let mut mc1 = Vec::new();
    let mut iops = Vec::new();
    let mut fs = Vec::new();
    let mut delayed = Vec::new();
    let mut stopped = 0;
    for r in runs {
        for (i, c) in r.cells.iter().enumerate() {
            stops.push(c.stops as f64);
            mc.push(c.max);
            iops.push(c.iops);
            delayed.push(c.delayed_writes as f64);
            if i == 0 {
                s1.push(c.stops as f64);
                mc1.push(c.max);
                if let Some(f) = c.first_stop_s {
                    fs.push(f);
                    stopped += 1;
                }
            }
        }
    }
    let opt = |f: fn(&RunReport) -> Option<f64>| Spread::of(runs.iter().filter_map(f).collect());
    Summary {
        runs: runs.len(),
        stops: Spread::of(stops),
        stops_cell1: Spread::of(s1),
        max_completion: Spread::of(mc),
        max_completion_cell1: Spread::of(mc1),
        iops: Spread::of(iops),
        first_stop: Spread::of(fs),
        first_cell_stopped: stopped,
        reclaim_at_entry: opt(|r| r.band_entry.map(|(rate, _)| rate / 1e6)),
        amp_pre_band: opt(|r| r.amp_pre_band),
        amp_total: Spread::of(runs.iter().map(|r| r.amp_total).collect()),
        delayed: Spread::of(delayed),
        first_condemn: opt(|r| r.first_condemn_s),
        first_delete: opt(|r| r.first_delete_s),
    }
}

impl Summary {
    /// One line per figure, `median (min-max)`.
    pub fn text(&self) -> String {
        let f =
            |s: &Spread, unit: &str| format!("{:.1} ({:.1}-{:.1}){unit}", s.median, s.min, s.max);
        format!(
            "runs {}\n  stops per cell        {}\n  stops, first cell     {}  (stopped: {}/{})\n  first stop at         {} s\n  max completion, cell  {} s\n  max completion, first {} s\n  iops per cell         {}\n  reclaim at band entry {} MB/s\n  amplification before the band {}  whole run {}\n  delayed writes/cell   {}\n  first condemn at      {} s   first delete at {} s",
            self.runs,
            f(&self.stops, ""),
            f(&self.stops_cell1, ""),
            self.first_cell_stopped,
            self.runs,
            f(&self.first_stop, ""),
            f(&self.max_completion, ""),
            f(&self.max_completion_cell1, ""),
            f(&self.iops, ""),
            f(&self.reclaim_at_entry, ""),
            f(&self.amp_pre_band, ""),
            f(&self.amp_total, ""),
            f(&self.delayed, ""),
            f(&self.first_condemn, ""),
            f(&self.first_delete, ""),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_orders_its_input() {
        let s = Spread::of(vec![3.0, 1.0, 2.0]);
        assert_eq!((s.min, s.median, s.max), (1.0, 2.0, 3.0));
        assert_eq!(Spread::of(vec![]).median, 0.0);
    }

    #[test]
    fn keys_with_the_policy_prefix_go_to_the_policy() {
        let sets = vec![
            ("cache_max".to_string(), "8G".to_string()),
            ("policy.grace_s".to_string(), "5".to_string()),
        ];
        let (sc, _) = build("ram-64k", "tunable", &sets).unwrap();
        assert_eq!(sc.cache_max, Some(8 << 30));
        assert!(build("ram-64k", "main", &[("policy.x".into(), "1".into())]).is_err());
    }
}
