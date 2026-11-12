//! Alignment and packing (M3 gate): every write is 4 KiB aligned in file
//! offset, length and buffer address, the files are open `O_DIRECT`, and
//! batch packing costs what DESIGN section 4.1 says.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use lunix_format::journal::RecordKind;
use lunix_iface::journal::SlotOutcome;
use lunix_journal::TraceEvent;

fn writes(ev: &Mutex<Vec<TraceEvent>>) -> Vec<(i32, u64, usize, usize)> {
    ev.lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter_map(|e| match e {
            TraceEvent::Write {
                fd,
                offset,
                len,
                addr,
                ..
            } => Some((*fd, *offset, *len, *addr)),
            _ => None,
        })
        .collect()
}

fn fd_flags(fd: i32) -> u32 {
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}")).expect("fdinfo");
    let line = info
        .lines()
        .find(|l| l.starts_with("flags:"))
        .expect("flags");
    u32::from_str_radix(line.trim_start_matches("flags:").trim(), 8).expect("octal")
}

type Rig = (
    tempfile::TempDir,
    lunix_journal::JournalConfig,
    Arc<Mutex<Vec<TraceEvent>>>,
);

fn setup(lanes: u16) -> Option<Rig> {
    let dir = scratch();
    if !o_direct_supported(dir.path()) {
        eprintln!("SKIP: the scratch file system refuses O_DIRECT");
        return None;
    }
    let mut cfg = small_cfg(dir.path().join("journal"), lanes, 8);
    let ev = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&ev);
    cfg.trace = Some(Arc::new(move |e: &TraceEvent| {
        sink.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(e.clone());
    }));
    Some((dir, cfg, ev))
}

#[test]
fn every_write_is_block_aligned_and_o_direct() {
    let Some((_dir, cfg, ev)) = setup(2) else {
        return;
    };
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut ls = [open_lane(&j, 0), open_lane(&j, 1)];
    let sizes = [1u32, 4, 16, 1, 128, 2, 7, 64, 1, 1, 1, 33];
    for (i, &b) in sizes.iter().cycle().take(60).enumerate() {
        let l = i % 2;
        write_wait(&mut *ls[l], i as u64 * 300, b);
        if i % 5 == 0 {
            // A hole too.
            if let SlotOutcome::Reserved(s) =
                ls[l].try_reserve(RecordKind::Hole, 99).expect("reserve")
            {
                ls[l].append(s, 5, &[]).expect("hole");
            }
        }
        // Poll often enough that the 4-buffer pool never runs dry, rarely
        // enough that records still pack.
        if i % 2 == 1 {
            let mut out = Vec::new();
            for lane in &mut ls {
                lane.poll(&mut out);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    for l in &mut ls {
        let mut out = Vec::new();
        let start = std::time::Instant::now();
        // Drain until the lane is idle (its watermark stops moving once every
        // append has completed, and equals its last accepted sequence number).
        let mut stable = 0;
        let mut prev = l.completed_watermark();
        while stable < 20 && start.elapsed() < T {
            l.poll(&mut out);
            std::thread::sleep(std::time::Duration::from_millis(1));
            let now = l.completed_watermark();
            stable = if now == prev { stable + 1 } else { 0 };
            prev = now;
        }
    }
    let w = writes(&ev);
    assert!(w.len() > 10, "{} writes", w.len());
    for &(fd, offset, len, addr) in &w {
        assert_eq!(offset % 4096, 0, "file offset");
        assert_eq!(len % 4096, 0, "length");
        assert_eq!(addr % 4096, 0, "buffer address");
        assert!(len > 0 && offset >= 4096, "never over the header block");
        assert_ne!(
            fd_flags(fd) & libc::O_DIRECT as u32,
            0,
            "fd {fd} is not O_DIRECT"
        );
    }
}

#[test]
fn packing_costs_what_the_design_says() {
    let Some((_dir, cfg, ev)) = setup(1) else {
        return;
    };
    let j = open_journal(&cfg);
    recover_all(&j);
    let mut l = open_lane(&j, 0);
    // 64 records appended without polling: the first is written alone (nothing
    // in flight), the rest pack into the batch that follows.
    let n = 64usize;
    for i in 0..n {
        write(&mut *l, i as u64, 1);
    }
    complete_appends(&mut *l, n);
    let blocks: usize = writes(&ev).iter().map(|w| w.2 / 4096).sum();
    let ideal = (n * 4160).div_ceil(4096);
    assert!(
        blocks <= ideal + 2,
        "{blocks} blocks written for {n} records, ideal {ideal}"
    );
    // An isolated record at queue depth 1 costs two blocks.
    let before = writes(&ev).len();
    write(&mut *l, 999, 1);
    complete_appends(&mut *l, 1);
    let w = writes(&ev);
    assert_eq!(w.len(), before + 1);
    assert_eq!(w[before].2, 8192);
}
