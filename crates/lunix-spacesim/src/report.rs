//! What a run reports: per cell figures, the per-second trace, the dead-time
//! split and the amplification.

use std::fmt::Write as _;

/// Latency buckets of one millisecond up to this many.
const BUCKETS: usize = 400_000;

/// One second of the trace.
#[derive(Clone, Debug, Default)]
pub struct SecRow {
    /// Seconds since the overwrite started.
    pub t: f64,
    /// The cell (1-based; 0 in a gap).
    pub cell: u32,
    /// Writes completed in the second.
    pub iops: f64,
    /// The longest completion latency of the second, ms.
    pub lat_max_ms: f64,
    /// The gate's state name.
    pub state: &'static str,
    /// The collector's level name.
    pub level: &'static str,
    /// Ledger `used`, bytes.
    pub used: u64,
    /// The limit, bytes.
    pub limit: u64,
    /// `limit - used`.
    pub headroom: u64,
    /// `JournalReserved`.
    pub journal: u64,
    /// Bytes on disk by the store model.
    pub disk: u64,
    /// Live bytes by the store model.
    pub live: u64,
    /// Condemned bytes.
    pub condemned: u64,
    /// Wholly dead and not yet condemned.
    pub dead_uncondemned: u64,
    /// The gate's 10 s ingest, bytes/s.
    pub ingest_rate: u64,
    /// The gate's 10 s reclaim, bytes/s.
    pub reclaim_rate: u64,
    /// Stops so far.
    pub stops: u64,
    /// Requests waiting in the gate.
    pub waiters: u64,
    /// Requests delayed so far.
    pub delayed: u64,
    /// Store bytes written per admitted journal byte over the last 10 s.
    pub a_hat: f64,
    /// The coordinator: `idle`, `cut`, `gc` or `urgent`.
    pub coord: &'static str,
    /// The collector phase.
    pub gc: &'static str,
}

/// A stop episode.
#[derive(Clone, Copy, Debug)]
pub struct StopEpisode {
    /// Start, seconds since the overwrite started.
    pub start: f64,
    /// Duration, seconds.
    pub secs: f64,
}

/// The figures of one cell (one fio run).
#[derive(Clone, Debug, Default)]
pub struct CellReport {
    /// 1-based.
    pub index: u32,
    /// Seconds.
    pub secs: f64,
    /// Mean writes per second.
    pub iops: f64,
    /// Writes per second in the first 10 s ("burst").
    pub burst_iops: f64,
    /// Writes per second after the first 10 s ("sustained").
    pub sustained_iops: f64,
    /// The slowest one second window, writes per second.
    pub min_1s_iops: f64,
    /// Latency percentiles and maximum, seconds.
    pub p50: f64,
    /// 99th percentile.
    pub p99: f64,
    /// 99.9th percentile.
    pub p999: f64,
    /// Maximum.
    pub max: f64,
    /// Times the gate entered `Stop` in the cell.
    pub stops: u64,
    /// Episodes of `Stop` that started in the cell.
    pub episodes: Vec<StopEpisode>,
    /// Time outside `Open`, seconds.
    pub throttled_s: f64,
    /// Requests the gate delayed.
    pub delayed_writes: u64,
    /// The longest gate wait since attach, ms (a maximum, not a delta).
    pub delay_max_ms: u64,
    /// Requests admitted by the delay cap.
    pub delay_capped: u64,
    /// Failed with `ENOSPC`.
    pub enospc: u64,
    /// Seconds from the cell start to the first stop.
    pub first_stop_s: Option<f64>,
    /// Peak `used`, bytes.
    pub peak_used: u64,
    /// The limit at the end, bytes.
    pub limit: u64,
    /// The longest wait for the journal ring, seconds.
    pub ring_wait_max: f64,
    /// Writes completed.
    pub writes: u64,
}

/// Dead time of the deleted segments, bytes-weighted means, seconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeadSummary {
    /// Segments deleted.
    pub segments: u64,
    /// Retention.
    pub retention: f64,
    /// Detection.
    pub detection: f64,
    /// Grace.
    pub grace: f64,
    /// Recheck.
    pub recheck: f64,
    /// From the recheck to the release of the bytes (the pass's index work).
    pub delete: f64,
}

/// A whole run.
#[derive(Clone, Debug, Default)]
pub struct RunReport {
    /// Scenario name.
    pub scenario: String,
    /// Policy name.
    pub policy: String,
    /// Seed.
    pub seed: u64,
    /// The cells.
    pub cells: Vec<CellReport>,
    /// The per-second trace.
    pub trace: Vec<SecRow>,
    /// Reclaim rate (bytes/s) the gate reported at its first entry into the
    /// band (or into `stop`), and when (seconds since the overwrite began).
    pub band_entry: Option<(f64, f64)>,
    /// Seconds from the overwrite's start to the first condemn.
    pub first_condemn_s: Option<f64>,
    /// Seconds from the overwrite's start to the first delete.
    pub first_delete_s: Option<f64>,
    /// Store bytes written by cuts per journal byte admitted, before the
    /// band.
    pub amp_pre_band: Option<f64>,
    /// The same over the whole run.
    pub amp_total: f64,
    /// Compaction bytes written per cut byte written.
    pub gc_wa: f64,
    /// Marks run.
    pub marks: u64,
    /// Urgent passes.
    pub urgent: u64,
    /// Seconds from the overwrite's start to the first stop.
    pub first_stop_abs: Option<f64>,
    /// Peak condemned list, bytes.
    pub peak_condemned: f64,
    /// Bytes condemned because every chunk died (whole-dead path).
    pub reclaim_whole: f64,
    /// Bytes condemned as compaction sources.
    pub reclaim_moved: f64,
    /// Compaction victims by live fraction, ten bins.
    pub victim_hist: [u64; 10],
    /// Collector time over the run's time.
    pub gc_share: f64,
    /// Dead time split.
    pub dead: DeadSummary,
    /// Peak `used` over the limit.
    pub peak_used_over_limit: f64,
    /// Figures of the first-fill window and the steady state.
    pub first_fill: WindowFigures,
    /// The rest of the run.
    pub steady: WindowFigures,
}

/// Figures of one window of the run.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowFigures {
    /// Seconds.
    pub secs: f64,
    /// Stops entered.
    pub stops: u64,
    /// Seconds in `Stop`.
    pub stop_secs: f64,
    /// Seconds in `Slowdown`.
    pub slowdown_secs: f64,
    /// Mean writes per second.
    pub iops: f64,
    /// The slowest second, writes per second.
    pub min_iops: f64,
    /// The longest completion latency, ms.
    pub lat_max_ms: f64,
}

/// A latency histogram of 1 ms buckets.
#[derive(Clone, Debug)]
pub struct Hist {
    b: Vec<u64>,
    n: u64,
    max_ms: f64,
}

impl Default for Hist {
    fn default() -> Self {
        Self {
            b: vec![0; BUCKETS],
            n: 0,
            max_ms: 0.0,
        }
    }
}

impl Hist {
    /// Records `n` completions of `ms` milliseconds.
    pub fn record(&mut self, ms: f64, n: u64) {
        let i = (ms.max(0.0) as usize).min(BUCKETS - 1);
        self.b[i] += n;
        self.n += n;
        self.max_ms = self.max_ms.max(ms);
    }

    /// The `q` quantile in seconds (upper edge of its bucket).
    pub fn quantile(&self, q: f64) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let want = (self.n as f64 * q).ceil().max(1.0) as u64;
        let mut acc = 0;
        for (i, c) in self.b.iter().enumerate() {
            acc += c;
            if acc >= want {
                return (i as f64 + 1.0) / 1000.0;
            }
        }
        self.max_ms / 1000.0
    }

    /// The maximum in seconds.
    pub fn max(&self) -> f64 {
        self.max_ms / 1000.0
    }
}

impl RunReport {
    /// The trace as CSV.
    pub fn trace_csv(&self) -> String {
        let mut s = String::from(
            "t,cell,iops,lat_max_ms,state,level,used,limit,headroom,journal,disk,live,condemned,dead_uncondemned,ingest_rate,reclaim_rate,stops,waiters,delayed,a_hat,coord,gc\n",
        );
        for r in &self.trace {
            let _ = writeln!(
                s,
                "{:.0},{},{:.0},{:.1},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.2},{},{}",
                r.t,
                r.cell,
                r.iops,
                r.lat_max_ms,
                r.state,
                r.level,
                r.used,
                r.limit,
                r.headroom,
                r.journal,
                r.disk,
                r.live,
                r.condemned,
                r.dead_uncondemned,
                r.ingest_rate,
                r.reclaim_rate,
                r.stops,
                r.waiters,
                r.delayed,
                r.a_hat,
                r.coord,
                r.gc
            );
        }
        s
    }

    /// The summary as text.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "scenario {}  policy {}  seed {}",
            self.scenario, self.policy, self.seed
        );
        let _ = writeln!(
            s,
            "cell  iops   burst  sustained  min1s   p50s   p99s  p99.9s    maxs  stops  stopS  throttledS  delayed  capped  firstStopS  ringWaitMax  peakUsed/limit"
        );
        for c in &self.cells {
            let stop_s: f64 = c.episodes.iter().map(|e| e.secs).sum();
            let _ = writeln!(
                s,
                "{:>4} {:>6.0} {:>7.0} {:>9.0} {:>6.0} {:>6.2} {:>6.2} {:>7.2} {:>7.1} {:>6} {:>6.1} {:>11.1} {:>8} {:>7} {:>11} {:>12.2} {:>14.3}",
                c.index,
                c.iops,
                c.burst_iops,
                c.sustained_iops,
                c.min_1s_iops,
                c.p50,
                c.p99,
                c.p999,
                c.max,
                c.stops,
                stop_s,
                c.throttled_s,
                c.delayed_writes,
                c.delay_capped,
                c.first_stop_s.map_or("-".into(), |v| format!("{v:.1}")),
                c.ring_wait_max,
                c.peak_used as f64 / c.limit.max(1) as f64
            );
            for e in &c.episodes {
                let _ = writeln!(s, "        stop at {:.1}s for {:.1}s", e.start, e.secs);
            }
        }
        if let Some((rate, when)) = self.band_entry {
            let _ = writeln!(
                s,
                "reclaim rate at first band entry: {:.0} MB/s (at {:.1}s)",
                rate / 1e6,
                when
            );
        }
        let f = |o: Option<f64>| o.map_or("-".to_string(), |v| format!("{v:.1}s"));
        let _ = writeln!(
            s,
            "first condemn {}  first delete {}  marks {}  urgent passes {}  peak condemned {:.2} GB",
            f(self.first_condemn_s),
            f(self.first_delete_s),
            self.marks,
            self.urgent,
            self.peak_condemned / 1e9
        );
        let _ = writeln!(
            s,
            "amplification (store bytes per admitted byte): before the band {}  whole run {:.2}  gc write amplification {:.2}",
            self.amp_pre_band.map_or("-".into(), |a| format!("{a:.2}")),
            self.amp_total,
            self.gc_wa
        );
        let _ = writeln!(
            s,
            "reclaim by path: whole-dead {:.2} GB, compaction sources {:.2} GB; collector busy {:.0} % of the time; victims by live fraction (0-10 % ... 90-100 %): {:?}",
            self.reclaim_whole / 1e9,
            self.reclaim_moved / 1e9,
            self.gc_share * 100.0,
            self.victim_hist
        );
        let d = &self.dead;
        let _ = writeln!(
            s,
            "dead time of {} deleted segments: retention {:.1}s + detection {:.1}s + grace {:.1}s + recheck {:.1}s",
            d.segments, d.retention, d.detection, d.grace, d.recheck
        );
        for (name, w) in [("first fill", &self.first_fill), ("steady", &self.steady)] {
            let _ = writeln!(
                s,
                "{name:>10}: {:>6.0}s  stops {:>2}  stopped {:>5.1}s  slowdown {:>5.1}s  iops {:>6.0}  worst second {:>6.0}  max latency {:>7.0} ms",
                w.secs, w.stops, w.stop_secs, w.slowdown_secs, w.iops, w.min_iops, w.lat_max_ms
            );
        }
        s
    }
}

/// Figures of the trace rows in `[from, to)` seconds.
pub fn window(trace: &[SecRow], from: f64, to: f64) -> WindowFigures {
    let rows: Vec<&SecRow> = trace
        .iter()
        .filter(|r| r.cell > 0 && r.t >= from && r.t < to)
        .collect();
    if rows.is_empty() {
        return WindowFigures::default();
    }
    let mut w = WindowFigures {
        secs: rows.len() as f64,
        min_iops: f64::MAX,
        ..WindowFigures::default()
    };
    let mut prev_stops = rows[0].stops;
    let first_stops = rows[0].stops;
    let mut sum = 0.0;
    for r in &rows {
        sum += r.iops;
        w.min_iops = w.min_iops.min(r.iops);
        w.lat_max_ms = w.lat_max_ms.max(r.lat_max_ms);
        match r.state {
            "stop" | "full" => w.stop_secs += 1.0,
            "slowdown" => w.slowdown_secs += 1.0,
            _ => {}
        }
        prev_stops = r.stops;
    }
    w.stops = prev_stops.saturating_sub(first_stops);
    w.iops = sum / rows.len() as f64;
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_come_from_the_buckets() {
        let mut h = Hist::default();
        h.record(2.0, 999);
        h.record(5000.0, 1);
        assert!((h.quantile(0.5) - 0.003).abs() < 1e-9);
        assert!((h.quantile(0.9995) - 5.001).abs() < 1e-6);
        assert!((h.max() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn a_window_counts_stops_and_seconds() {
        let rows: Vec<SecRow> = (0..10)
            .map(|i| SecRow {
                t: f64::from(i),
                cell: 1,
                iops: if i < 5 { 100.0 } else { 0.0 },
                state: if i < 5 { "open" } else { "stop" },
                stops: u64::from(i >= 5),
                ..SecRow::default()
            })
            .collect();
        let w = window(&rows, 0.0, 10.0);
        assert_eq!(w.stop_secs, 5.0);
        assert_eq!(w.stops, 1);
        assert_eq!(w.min_iops, 0.0);
        assert!((w.iops - 50.0).abs() < 1e-9);
    }
}
