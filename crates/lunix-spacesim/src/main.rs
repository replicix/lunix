//! `spacesim`: the space-gate simulator.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use lunix_spacesim::calibration::{build, run_seeds, sets_from_toml, summarize};
use lunix_spacesim::params::SCENARIO_NAMES;
use lunix_spacesim::{run, POLICY_NAMES};

#[derive(Parser)]
#[command(name = "spacesim", about = "Simulator of the space gate (ADR-32)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the scenarios and policies.
    List,
    /// One run: a summary, and with --trace a per-second CSV.
    Run {
        #[arg(short, long, default_value = "ram-64k")]
        scenario: String,
        #[arg(short, long, default_value = "main")]
        policy: String,
        /// key=value overrides (`policy.key` for the policy); repeatable.
        #[arg(long = "set")]
        sets: Vec<String>,
        /// A flat TOML parameter file.
        #[arg(long)]
        params: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Write the per-second trace here.
        #[arg(long)]
        trace: Option<PathBuf>,
    },
    /// Several seeds, with the phase of the collector's cadence drawn from
    /// each: median and range.
    Seeds {
        #[arg(short, long, default_value = "ram-64k")]
        scenario: String,
        #[arg(short, long, default_value = "main")]
        policy: String,
        #[arg(long = "set")]
        sets: Vec<String>,
        #[arg(long)]
        params: Option<PathBuf>,
        #[arg(short, long, default_value_t = 5)]
        n: u64,
        /// Keep the preset's phase instead of drawing it.
        #[arg(long)]
        fixed_phase: bool,
    },
    /// A grid over parameters: `--grid key=v1,v2,...` (repeatable).
    Sweep {
        #[arg(short, long, default_value = "ram-64k")]
        scenario: String,
        #[arg(short, long, default_value = "main")]
        policy: String,
        #[arg(long = "set")]
        sets: Vec<String>,
        #[arg(long = "grid")]
        grids: Vec<String>,
        #[arg(short, long, default_value_t = 5)]
        n: u64,
    },
    /// The candidate matrix as markdown: every candidate over seeds at two
    /// store-amplification levels (`merge` 1.0 and 0.67).
    Experiments {
        #[arg(short, long, default_value = "ram-64k")]
        scenario: String,
        #[arg(short, long, default_value_t = 8)]
        n: u64,
        /// The merge factor of the amplification level.
        #[arg(long, default_value = "1.0")]
        merge: String,
    },
    /// The calibration table: `main` on the measured cells, and the two
    /// null-result policies beside it.
    Calibrate {
        #[arg(short, long, default_value_t = 5)]
        n: u64,
    },
}

fn kv(s: &str) -> Result<(String, String)> {
    let (k, v) = s
        .split_once('=')
        .with_context(|| format!("expected key=value: {s}"))?;
    Ok((k.trim().to_string(), v.trim().to_string()))
}

fn collect_sets(sets: &[String], params: &Option<PathBuf>) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    if let Some(p) = params {
        let text = fs::read_to_string(p).with_context(|| p.display().to_string())?;
        out.extend(sets_from_toml(&text)?);
    }
    for s in sets {
        out.push(kv(s)?);
    }
    Ok(out)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::List => {
            println!("scenarios: {}", SCENARIO_NAMES.join(", "));
            println!("policies:  {}", POLICY_NAMES.join(", "));
        }
        Cmd::Run {
            scenario,
            policy,
            sets,
            params,
            seed,
            trace,
        } => {
            let sets = collect_sets(&sets, &params)?;
            let (mut sc, mut pol) = build(&scenario, &policy, &sets)?;
            sc.seed = seed;
            let rep = run(&sc, pol.as_mut())?;
            print!("{}", rep.summary());
            if let Some(t) = trace {
                fs::write(&t, rep.trace_csv()).with_context(|| t.display().to_string())?;
            }
        }
        Cmd::Seeds {
            scenario,
            policy,
            sets,
            params,
            n,
            fixed_phase,
        } => {
            let sets = collect_sets(&sets, &params)?;
            let runs = run_seeds(&scenario, &policy, &sets, n, !fixed_phase)?;
            println!("{scenario} / {policy}");
            println!("{}", summarize(&runs).text());
        }
        Cmd::Sweep {
            scenario,
            policy,
            sets,
            grids,
            n,
        } => {
            let base = sets.iter().map(|s| kv(s)).collect::<Result<Vec<_>>>()?;
            let mut axes: Vec<(String, Vec<String>)> = Vec::new();
            for g in &grids {
                let (k, v) = kv(g)?;
                axes.push((k, v.split(',').map(|x| x.trim().to_string()).collect()));
            }
            if axes.is_empty() {
                bail!("no --grid given");
            }
            println!(
                "{:<40} {:>7} {:>9} {:>10} {:>9} {:>9}",
                "point", "stops", "stops(c1)", "maxcompl", "iops", "stopped"
            );
            let mut idx = vec![0usize; axes.len()];
            loop {
                let mut sets = base.clone();
                let mut label = Vec::new();
                for (a, &i) in axes.iter().zip(&idx) {
                    sets.push((a.0.clone(), a.1[i].clone()));
                    label.push(format!("{}={}", a.0, a.1[i]));
                }
                let runs = run_seeds(&scenario, &policy, &sets, n, true)?;
                let s = summarize(&runs);
                println!(
                    "{:<40} {:>7.1} {:>9.1} {:>10.1} {:>9.0} {:>6}/{}",
                    label.join(" "),
                    s.stops.median,
                    s.stops_cell1.median,
                    s.max_completion.median,
                    s.iops.median,
                    s.first_cell_stopped,
                    s.runs
                );
                let mut k = 0;
                loop {
                    if k == axes.len() {
                        return Ok(());
                    }
                    idx[k] += 1;
                    if idx[k] < axes[k].1.len() {
                        break;
                    }
                    idx[k] = 0;
                    k += 1;
                }
            }
        }
        Cmd::Experiments { scenario, n, merge } => {
            print!(
                "{}",
                lunix_spacesim::experiments::table(&scenario, &merge, n)?
            );
        }
        Cmd::Calibrate { n } => {
            for scenario in SCENARIO_NAMES {
                for policy in ["main", "gate-gc", "leadzone"] {
                    let runs = run_seeds(scenario, policy, &[], n, true)?;
                    println!("== {scenario} / {policy}");
                    println!("{}", summarize(&runs).text());
                }
            }
        }
    }
    Ok(())
}
