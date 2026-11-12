//! `journal-lane-bench`: closed-loop append + group-flush benchmark of
//! `lunix-journal` (M3 perf gate; compare with `spikes/journal-bench` group
//! commit at batch >= 32 in the same session).
//!
//! One thread per lane appends a group of records, waits for their
//! completions, asks for a group flush and waits for it; a background thread
//! plays the destage worker and reclaims what every lane has flushed.
//!
//! ```text
//! journal-lane-bench --dir DIR [--lanes 1] [--rec 4096] [--group 64] [--secs 5]
//!                    [--journal-mib 256] [--max-inflight 2] [--pool 8] [--keep]
//! ```
//!
//! Prints one JSON line.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use lunix_format::journal::RecordKind;
use lunix_format::{VolumeUuid, WriteSeq};
use lunix_iface::journal::{Journal, JournalCompletion, LaneId, SlotOutcome};
use lunix_iface::wake::WakeFd;
use lunix_journal::{FileJournal, JournalConfig, TraceEvent};
use std::sync::Mutex;

struct Args {
    dir: PathBuf,
    lanes: u16,
    rec: usize,
    group: usize,
    secs: f64,
    journal_mib: u64,
    max_inflight: usize,
    pool: usize,
    keep: bool,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        dir: PathBuf::new(),
        lanes: 1,
        rec: 4096,
        group: 64,
        secs: 5.0,
        journal_mib: 0,
        max_inflight: 2,
        pool: 8,
        keep: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match k.as_str() {
            "--dir" => a.dir = PathBuf::from(val("--dir")?),
            "--lanes" => a.lanes = val("--lanes")?.parse().map_err(|e| format!("{e}"))?,
            "--rec" => a.rec = val("--rec")?.parse().map_err(|e| format!("{e}"))?,
            "--group" => a.group = val("--group")?.parse().map_err(|e| format!("{e}"))?,
            "--secs" => a.secs = val("--secs")?.parse().map_err(|e| format!("{e}"))?,
            "--journal-mib" => {
                a.journal_mib = val("--journal-mib")?.parse().map_err(|e| format!("{e}"))?;
            }
            "--max-inflight" => {
                a.max_inflight = val("--max-inflight")?.parse().map_err(|e| format!("{e}"))?;
            }
            "--pool" => a.pool = val("--pool")?.parse().map_err(|e| format!("{e}"))?,
            "--keep" => a.keep = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.dir.as_os_str().is_empty() {
        return Err("--dir is required".into());
    }
    if a.rec == 0 || a.group == 0 || a.lanes == 0 {
        return Err("--rec, --group and --lanes must be positive".into());
    }
    if a.journal_mib == 0 {
        a.journal_mib = 256 * u64::from(a.lanes);
    }
    Ok(a)
}

#[derive(Default)]
struct Stats {
    records: u64,
    flushes: u64,
    flush_ns: Vec<u64>,
    group_ns: Vec<u64>,
}

fn wait_wake(wake: &WakeFd, ms: i32) {
    let mut p = libc::pollfd {
        fd: wake.raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `p` is a live pollfd and the count is 1.
    unsafe { libc::poll(&mut p, 1, ms) };
    wake.drain();
}

fn lane_thread(
    j: Arc<FileJournal>,
    lane_id: u16,
    a: &Args,
    deadline: Instant,
    flushed: Arc<Vec<AtomicU64>>,
) -> Stats {
    let wake = WakeFd::new().expect("eventfd");
    let mut lane = j
        .open_lane(LaneId(lane_id), wake.clone())
        .expect("open lane");
    let blocks = a.rec.div_ceil(4096) as u32;
    let data = vec![0x5au8; blocks as usize * 4096];
    let mut st = Stats::default();
    let mut out = Vec::new();
    while Instant::now() < deadline {
        let t0 = Instant::now();
        let mut done = 0usize;
        let mut last = 0u64;
        for _ in 0..a.group {
            loop {
                match lane
                    .try_reserve(RecordKind::Write, blocks)
                    .expect("reserve")
                {
                    SlotOutcome::Reserved(s) => {
                        last = s.write_seq.0;
                        lane.append(s, 0, &data).expect("append");
                        break;
                    }
                    SlotOutcome::Full | SlotOutcome::NoSpace => {
                        out.clear();
                        lane.poll(&mut out);
                        done += count_ok(&out);
                        if out.is_empty() {
                            wait_wake(&wake, 1);
                        }
                    }
                }
            }
        }
        while done < a.group {
            out.clear();
            lane.poll(&mut out);
            done += count_ok(&out);
            if done < a.group {
                wait_wake(&wake, 5);
            }
        }
        let t1 = Instant::now();
        let (tx, rx) = mpsc::channel();
        j.submit_group_flush(
            j.last_issued(),
            Box::new(move |r| {
                let _ = tx.send(r.is_ok());
            }),
        )
        .expect("submit flush");
        assert!(rx.recv().expect("flush"), "flush failed");
        let t2 = Instant::now();
        flushed[usize::from(lane_id)].store(last, Ordering::Release);
        st.records += a.group as u64;
        st.flushes += 1;
        st.flush_ns.push((t2 - t1).as_nanos() as u64);
        st.group_ns.push((t2 - t0).as_nanos() as u64);
    }
    st
}

fn count_ok(out: &[JournalCompletion]) -> usize {
    out.iter()
        .filter(|c| {
            matches!(c, JournalCompletion::Append { status, .. } if {
                assert!(status.is_ok(), "append failed");
                true
            })
        })
        .count()
}

fn pct(v: &mut [u64], q: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable();
    let i = ((v.len() as f64 - 1.0) * q).round() as usize;
    v[i] as f64 / 1000.0
}

fn main() {
    let a = match parse() {
        Ok(a) => Arc::new(a),
        Err(e) => {
            eprintln!("journal-lane-bench: {e}");
            std::process::exit(2);
        }
    };
    let mut cfg = JournalConfig::new(
        a.dir.join("journal"),
        VolumeUuid([9; 16]),
        a.lanes,
        a.journal_mib << 20,
    );
    cfg.max_inflight = a.max_inflight;
    cfg.pool_buffers = a.pool;
    // The coordinator's own time per group (watermark wait excluded): from the
    // moment the group formed to the last fdatasync completing, which is what
    // the spike's "flush" latency measures.
    let sync_ns: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let group_start = Arc::new(Mutex::new(Instant::now()));
    {
        let (sync_ns, group_start) = (Arc::clone(&sync_ns), Arc::clone(&group_start));
        cfg.trace = Some(Arc::new(move |e: &TraceEvent| match e {
            TraceEvent::GroupStart { .. } => {
                *group_start.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
            }
            TraceEvent::GroupDone { .. } => {
                let t = group_start
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .elapsed();
                sync_ns
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(t.as_nanos() as u64);
            }
            _ => {}
        }));
    }
    let t_setup = Instant::now();
    let j = Arc::new(FileJournal::open(cfg).expect("open journal"));
    j.recover(None, WriteSeq(0)).expect("recover");
    let setup = t_setup.elapsed();

    let flushed: Arc<Vec<AtomicU64>> = Arc::new((0..a.lanes).map(|_| AtomicU64::new(0)).collect());
    let stop = Arc::new(AtomicBool::new(false));
    let reclaimed = Arc::new(AtomicU64::new(0));
    let destage = {
        let (j, flushed, stop, reclaimed) = (
            Arc::clone(&j),
            Arc::clone(&flushed),
            Arc::clone(&stop),
            Arc::clone(&reclaimed),
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(50));
                let cut = flushed
                    .iter()
                    .map(|f| f.load(Ordering::Acquire))
                    .min()
                    .unwrap_or(0);
                if cut == 0 {
                    continue;
                }
                if let Ok(tail) = j.tail_after(WriteSeq(cut)) {
                    if let Ok(r) = j.reclaim(&tail) {
                        reclaimed.fetch_add(u64::from(r.files), Ordering::Relaxed);
                    }
                }
            }
        })
    };

    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(a.secs);
    let handles: Vec<_> = (0..a.lanes)
        .map(|l| {
            let (j, a, f) = (Arc::clone(&j), Arc::clone(&a), Arc::clone(&flushed));
            std::thread::spawn(move || lane_thread(j, l, &a, deadline, f))
        })
        .collect();
    let mut total = Stats::default();
    for h in handles {
        let s = h.join().expect("lane thread");
        total.records += s.records;
        total.flushes += s.flushes;
        total.flush_ns.extend(s.flush_ns);
        total.group_ns.extend(s.group_ns);
    }
    let wall = start.elapsed().as_secs_f64();
    stop.store(true, Ordering::Release);
    let _ = destage.join();
    let mut sync_v: Vec<u64> =
        std::mem::take(&mut *sync_ns.lock().unwrap_or_else(|p| p.into_inner()));
    println!(
        "{{\"bench\":\"journal-lane-bench\",\"dir\":\"{}\",\"lanes\":{},\"rec\":{},\"group\":{},\"max_inflight\":{},\"pool\":{},\"secs\":{:.3},\"setup_s\":{:.2},\"records\":{},\"rec_per_s\":{:.0},\"mb_per_s\":{:.1},\"flushes\":{},\"rec_per_flush\":{:.2},\"sync_p50_us\":{:.1},\"sync_p99_us\":{:.1},\"flush_e2e_p50_us\":{:.1},\"flush_e2e_p99_us\":{:.1},\"group_p50_us\":{:.1},\"group_p99_us\":{:.1},\"files_reclaimed\":{}}}",
        a.dir.display(),
        a.lanes,
        a.rec,
        a.group,
        a.max_inflight,
        a.pool,
        wall,
        setup.as_secs_f64(),
        total.records,
        total.records as f64 / wall,
        total.records as f64 * a.rec as f64 / wall / 1e6,
        total.flushes,
        total.records as f64 / total.flushes.max(1) as f64,
        pct(&mut sync_v, 0.5),
        pct(&mut sync_v, 0.99),
        pct(&mut total.flush_ns, 0.5),
        pct(&mut total.flush_ns, 0.99),
        pct(&mut total.group_ns, 0.5),
        pct(&mut total.group_ns, 0.99),
        reclaimed.load(Ordering::Relaxed),
    );
    drop(j);
    if !a.keep {
        let _ = std::fs::remove_dir_all(a.dir.join("journal"));
    }
}
